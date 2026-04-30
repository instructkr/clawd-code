#![allow(dead_code)]
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::session::{Session, SessionError};

/// Per-worktree session store that namespaces on-disk session files by
/// workspace fingerprint so that parallel `opencode serve` instances never
/// collide.
///
/// Create via [`SessionStore::from_cwd`] (derives the store path from the
/// server's working directory) or [`SessionStore::from_data_dir`] (honours an
/// explicit `--data-dir` flag).  Both constructors produce a directory layout
/// of `<data_dir>/sessions/<workspace_hash>/` where `<workspace_hash>` is a
/// stable hex digest of the canonical workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStore {
    /// Resolved root of the session namespace, e.g.
    /// `/home/user/project/.claw/sessions/a1b2c3d4e5f60718/`.
    sessions_root: PathBuf,
    /// The canonical workspace path that was fingerprinted.
    workspace_root: PathBuf,
}

impl SessionStore {
    /// Build a store from the server's current working directory.
    ///
    /// The on-disk layout becomes `<cwd>/.claw/sessions/<workspace_hash>/`.
    pub fn from_cwd(cwd: impl AsRef<Path>) -> Result<Self, SessionControlError> {
        let cwd = cwd.as_ref();
        // #151: canonicalize so equivalent paths (symlinks, relative vs
        // absolute, /tmp vs /private/tmp on macOS) produce the same
        // workspace_fingerprint. Falls back to the raw path if canonicalize
        // fails (e.g. the directory doesn't exist yet).
        let canonical_cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let sessions_root = canonical_cwd
            .join(".claw")
            .join("sessions")
            .join(workspace_fingerprint(&canonical_cwd));
        fs::create_dir_all(&sessions_root)?;
        Ok(Self {
            sessions_root,
            workspace_root: canonical_cwd,
        })
    }

    /// Build a store from an explicit `--data-dir` flag.
    ///
    /// The on-disk layout becomes `<data_dir>/sessions/<workspace_hash>/`
    /// where `<workspace_hash>` is derived from `workspace_root`.
    pub fn from_data_dir(
        data_dir: impl AsRef<Path>,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Self, SessionControlError> {
        let workspace_root = workspace_root.as_ref();
        // #151: canonicalize workspace_root for consistent fingerprinting
        // across equivalent path representations.
        let canonical_workspace =
            fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let sessions_root = data_dir
            .as_ref()
            .join("sessions")
            .join(workspace_fingerprint(&canonical_workspace));
        fs::create_dir_all(&sessions_root)?;
        Ok(Self {
            sessions_root,
            workspace_root: canonical_workspace,
        })
    }

    /// The fully resolved sessions directory for this namespace.
    #[must_use]
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_root
    }

    /// The workspace root this store is bound to.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn create_handle(&self, session_id: &str) -> SessionHandle {
        let id = session_id.to_string();
        let path = self
            .sessions_root
            .join(format!("{id}.{PRIMARY_SESSION_EXTENSION}"));
        SessionHandle { id, path }
    }

    pub fn resolve_reference(&self, reference: &str) -> Result<SessionHandle, SessionControlError> {
        if is_session_reference_alias(reference) {
            let latest = self.latest_session()?;
            return Ok(SessionHandle {
                id: latest.id,
                path: latest.path,
            });
        }

        let direct = PathBuf::from(reference);
        let candidate = if direct.is_absolute() {
            direct.clone()
        } else {
            self.workspace_root.join(&direct)
        };
        let looks_like_path = direct.extension().is_some() || direct.components().count() > 1;
        let path = if candidate.exists() {
            candidate
        } else if looks_like_path {
            return Err(SessionControlError::Format(
                format_missing_session_reference(reference, &self.sessions_root),
            ));
        } else {
            self.resolve_managed_path(reference)?
        };

        Ok(SessionHandle {
            id: session_id_from_path(&path).unwrap_or_else(|| reference.to_string()),
            path,
        })
    }

    pub fn resolve_managed_path(&self, session_id: &str) -> Result<PathBuf, SessionControlError> {
        for extension in [PRIMARY_SESSION_EXTENSION, LEGACY_SESSION_EXTENSION] {
            let path = self.sessions_root.join(format!("{session_id}.{extension}"));
            if path.exists() {
                return Ok(path);
            }
        }
        if let Some(legacy_root) = self.legacy_sessions_root() {
            for extension in [PRIMARY_SESSION_EXTENSION, LEGACY_SESSION_EXTENSION] {
                let path = legacy_root.join(format!("{session_id}.{extension}"));
                if !path.exists() {
                    continue;
                }
                let session = Session::load_from_path(&path)?;
                self.validate_loaded_session(&path, &session)?;
                return Ok(path);
            }
        }
        Err(SessionControlError::Format(
            format_missing_session_reference(session_id, &self.sessions_root),
        ))
    }

    pub fn list_sessions(&self) -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
        let mut sessions = Vec::new();
        self.collect_sessions_from_dir(&self.sessions_root, &mut sessions)?;
        if let Some(legacy_root) = self.legacy_sessions_root() {
            self.collect_sessions_from_dir(&legacy_root, &mut sessions)?;
        }
        sort_managed_sessions(&mut sessions);
        Ok(sessions)
    }

    pub fn latest_session(&self) -> Result<ManagedSessionSummary, SessionControlError> {
        if let Some(latest) = self.list_sessions()?.into_iter().next() {
            return Ok(latest);
        }
        if let Some(latest) = self.scan_global_sessions()?.into_iter().next() {
            return Ok(latest);
        }
        Err(SessionControlError::Format(format_no_managed_sessions(
            &self.sessions_root,
        )))
    }

    pub fn load_session(
        &self,
        reference: &str,
    ) -> Result<LoadedManagedSession, SessionControlError> {
        let handle = self.resolve_reference(reference)?;
        let session = Session::load_from_path(&handle.path)?;
        self.validate_loaded_session(&handle.path, &session)?;
        Ok(LoadedManagedSession {
            handle: SessionHandle {
                id: session.session_id.clone(),
                path: handle.path,
            },
            session,
        })
    }

    /// Load a session by reference, allowing cross-workspace resume for aliases.
    /// When the reference is an alias ("latest", "last", "recent"), workspace
    /// mismatch validation is skipped so `/resume latest` works across workspaces.
    /// For explicit session references, workspace validation is still enforced.
    pub fn load_session_loose(
        &self,
        reference: &str,
    ) -> Result<LoadedManagedSession, SessionControlError> {
        match self.load_session(reference) {
            Ok(loaded) => Ok(loaded),
            Err(SessionControlError::WorkspaceMismatch { expected, actual })
                if is_session_reference_alias(reference) =>
            {
                let handle = self.resolve_reference(reference)?;
                let session = Session::load_from_path(&handle.path)?;
                eprintln!(
                    "  Note: resuming session from a different workspace (origin: {})",
                    actual.display()
                );
                let _ = expected; // suppress unused warning
                Ok(LoadedManagedSession {
                    handle: SessionHandle {
                        id: session.session_id.clone(),
                        path: handle.path,
                    },
                    session,
                })
            }
            Err(other) => Err(other),
        }
    }

    pub fn fork_session(
        &self,
        session: &Session,
        branch_name: Option<String>,
    ) -> Result<ForkedManagedSession, SessionControlError> {
        let parent_session_id = session.session_id.clone();
        let forked = session
            .fork(branch_name)
            .with_workspace_root(self.workspace_root.clone());
        let handle = self.create_handle(&forked.session_id);
        let branch_name = forked
            .fork
            .as_ref()
            .and_then(|fork| fork.branch_name.clone());
        let forked = forked.with_persistence_path(handle.path.clone());
        forked.save_to_path(&handle.path)?;
        Ok(ForkedManagedSession {
            parent_session_id,
            handle,
            session: forked,
            branch_name,
        })
    }

    fn legacy_sessions_root(&self) -> Option<PathBuf> {
        self.sessions_root
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == "sessions"))
            .map(Path::to_path_buf)
    }

    /// Scan all known session storage locations for sessions from any workspace.
    /// Checks both the global root (~/.claw/sessions/) and the project-local
    /// .claw/sessions/ parent directory. Used as a fallback when the current
    /// workspace has no sessions.
    #[allow(clippy::unnecessary_wraps)]
    fn scan_global_sessions(&self) -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
        let mut sessions = Vec::new();

        // Scan global root: ~/.claw/sessions/<fingerprint>/
        let global_root = global_sessions_root();
        if let Ok(entries) = fs::read_dir(&global_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = Self::collect_sessions_from_dir_unvalidated(&path, &mut sessions);
                }
            }
        }

        // Scan project-local parent: <cwd>/.claw/sessions/<fingerprint>/
        // Sessions are stored here by from_cwd(), so we must check all
        // fingerprint subdirs, not just the current workspace's.
        if let Some(local_parent) = self.legacy_sessions_root() {
            if let Ok(entries) = fs::read_dir(&local_parent) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path != self.sessions_root {
                        let _ = Self::collect_sessions_from_dir_unvalidated(&path, &mut sessions);
                    } else if path == self.sessions_root {
                        // Already searched in list_sessions(), but include here
                        // in case this is called standalone
                        let _ = Self::collect_sessions_from_dir_unvalidated(&path, &mut sessions);
                    }
                }
            }
        }

        sort_managed_sessions(&mut sessions);
        Ok(sessions)
    }

    fn validate_loaded_session(
        &self,
        session_path: &Path,
        session: &Session,
    ) -> Result<(), SessionControlError> {
        let Some(actual) = session.workspace_root() else {
            if path_is_within_workspace(session_path, &self.workspace_root) {
                return Ok(());
            }
            return Err(SessionControlError::Format(
                format_legacy_session_missing_workspace_root(session_path, &self.workspace_root),
            ));
        };
        if workspace_roots_match(actual, &self.workspace_root) {
            return Ok(());
        }
        Err(SessionControlError::WorkspaceMismatch {
            expected: self.workspace_root.clone(),
            actual: actual.to_path_buf(),
        })
    }

    fn collect_sessions_from_dir(
        &self,
        directory: &Path,
        sessions: &mut Vec<ManagedSessionSummary>,
    ) -> Result<(), SessionControlError> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !is_managed_session_file(&path) {
                continue;
            }
            let metadata = entry.metadata()?;
            let modified_epoch_millis = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            let summary = match Session::load_from_path(&path) {
                Ok(session) => {
                    if self.validate_loaded_session(&path, &session).is_err() {
                        continue;
                    }
                    ManagedSessionSummary {
                        id: session.session_id,
                        path,
                        updated_at_ms: session.updated_at_ms,
                        modified_epoch_millis,
                        message_count: session.messages.len(),
                        parent_session_id: session
                            .fork
                            .as_ref()
                            .map(|fork| fork.parent_session_id.clone()),
                        branch_name: session
                            .fork
                            .as_ref()
                            .and_then(|fork| fork.branch_name.clone()),
                    }
                }
                Err(_) => ManagedSessionSummary {
                    id: path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    path,
                    updated_at_ms: 0,
                    modified_epoch_millis,
                    message_count: 0,
                    parent_session_id: None,
                    branch_name: None,
                },
            };
            sessions.push(summary);
        }
        Ok(())
    }

    /// Like `collect_sessions_from_dir` but skips workspace validation.
    /// Used by the global scan fallback to discover sessions from any workspace.
    fn collect_sessions_from_dir_unvalidated(
        directory: &Path,
        sessions: &mut Vec<ManagedSessionSummary>,
    ) -> Result<(), SessionControlError> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !is_managed_session_file(&path) {
                continue;
            }
            let metadata = entry.metadata()?;
            let modified_epoch_millis = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            let summary = match Session::load_from_path(&path) {
                Ok(session) => ManagedSessionSummary {
                    id: session.session_id,
                    path,
                    updated_at_ms: session.updated_at_ms,
                    modified_epoch_millis,
                    message_count: session.messages.len(),
                    parent_session_id: session
                        .fork
                        .as_ref()
                        .map(|fork| fork.parent_session_id.clone()),
                    branch_name: session
                        .fork
                        .as_ref()
                        .and_then(|fork| fork.branch_name.clone()),
                },
                Err(_) => ManagedSessionSummary {
                    id: path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    path,
                    updated_at_ms: 0,
                    modified_epoch_millis,
                    message_count: 0,
                    parent_session_id: None,
                    branch_name: None,
                },
            };
            sessions.push(summary);
        }
        Ok(())
    }
}

/// Stable hex fingerprint of a workspace path.
///
/// Uses FNV-1a (64-bit) to produce a 16-char hex string that partitions the
/// on-disk session directory per workspace root.
#[must_use]
pub fn workspace_fingerprint(workspace_root: &Path) -> String {
    let input = workspace_root.to_string_lossy();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The global sessions directory shared across all workspaces.
/// Points to `~/.claw/sessions/` (or `$CLAW_CONFIG_HOME/sessions/`).
#[must_use]
pub fn global_sessions_root() -> PathBuf {
    crate::config::default_config_home().join("sessions")
}

pub const PRIMARY_SESSION_EXTENSION: &str = "jsonl";
pub const LEGACY_SESSION_EXTENSION: &str = "json";
pub const LATEST_SESSION_REFERENCE: &str = "latest";

const SESSION_REFERENCE_ALIASES: &[&str] = &[LATEST_SESSION_REFERENCE, "last", "recent"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandle {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub updated_at_ms: u64,
    pub modified_epoch_millis: u128,
    pub message_count: usize,
    pub parent_session_id: Option<String>,
    pub branch_name: Option<String>,
}

fn sort_managed_sessions(sessions: &mut [ManagedSessionSummary]) {
    sessions.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.modified_epoch_millis.cmp(&left.modified_epoch_millis))
            .then_with(|| right.id.cmp(&left.id))
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedManagedSession {
    pub handle: SessionHandle,
    pub session: Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkedManagedSession {
    pub parent_session_id: String,
    pub handle: SessionHandle,
    pub session: Session,
    pub branch_name: Option<String>,
}

#[derive(Debug)]
pub enum SessionControlError {
    Io(std::io::Error),
    Session(SessionError),
    Format(String),
    WorkspaceMismatch { expected: PathBuf, actual: PathBuf },
}

impl Display for SessionControlError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Session(error) => write!(f, "{error}"),
            Self::Format(error) => write!(f, "{error}"),
            Self::WorkspaceMismatch { expected, actual } => write!(
                f,
                "session workspace mismatch: expected {}, found {}",
                expected.display(),
                actual.display()
            ),
        }
    }
}

impl std::error::Error for SessionControlError {}

impl From<std::io::Error> for SessionControlError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SessionError> for SessionControlError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

pub fn sessions_dir() -> Result<PathBuf, SessionControlError> {
    managed_sessions_dir_for(env::current_dir()?)
}

pub fn managed_sessions_dir_for(
    base_dir: impl AsRef<Path>,
) -> Result<PathBuf, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    Ok(store.sessions_dir().to_path_buf())
}

pub fn create_managed_session_handle(
    session_id: &str,
) -> Result<SessionHandle, SessionControlError> {
    create_managed_session_handle_for(env::current_dir()?, session_id)
}

pub fn create_managed_session_handle_for(
    base_dir: impl AsRef<Path>,
    session_id: &str,
) -> Result<SessionHandle, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    Ok(store.create_handle(session_id))
}

pub fn resolve_session_reference(reference: &str) -> Result<SessionHandle, SessionControlError> {
    resolve_session_reference_for(env::current_dir()?, reference)
}

pub fn resolve_session_reference_for(
    base_dir: impl AsRef<Path>,
    reference: &str,
) -> Result<SessionHandle, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.resolve_reference(reference)
}

pub fn resolve_managed_session_path(session_id: &str) -> Result<PathBuf, SessionControlError> {
    resolve_managed_session_path_for(env::current_dir()?, session_id)
}

pub fn resolve_managed_session_path_for(
    base_dir: impl AsRef<Path>,
    session_id: &str,
) -> Result<PathBuf, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.resolve_managed_path(session_id)
}

#[must_use]
pub fn is_managed_session_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|extension| {
            extension == PRIMARY_SESSION_EXTENSION || extension == LEGACY_SESSION_EXTENSION
        })
}

pub fn list_managed_sessions() -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
    list_managed_sessions_for(env::current_dir()?)
}

pub fn list_managed_sessions_for(
    base_dir: impl AsRef<Path>,
) -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.list_sessions()
}

pub fn latest_managed_session() -> Result<ManagedSessionSummary, SessionControlError> {
    latest_managed_session_for(env::current_dir()?)
}

pub fn latest_managed_session_for(
    base_dir: impl AsRef<Path>,
) -> Result<ManagedSessionSummary, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.latest_session()
}

pub fn load_managed_session(reference: &str) -> Result<LoadedManagedSession, SessionControlError> {
    load_managed_session_for(env::current_dir()?, reference)
}

pub fn load_managed_session_for(
    base_dir: impl AsRef<Path>,
    reference: &str,
) -> Result<LoadedManagedSession, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.load_session(reference)
}

pub fn fork_managed_session(
    session: &Session,
    branch_name: Option<String>,
) -> Result<ForkedManagedSession, SessionControlError> {
    fork_managed_session_for(env::current_dir()?, session, branch_name)
}

pub fn fork_managed_session_for(
    base_dir: impl AsRef<Path>,
    session: &Session,
    branch_name: Option<String>,
) -> Result<ForkedManagedSession, SessionControlError> {
    let store = SessionStore::from_cwd(base_dir)?;
    store.fork_session(session, branch_name)
}

#[must_use]
pub fn is_session_reference_alias(reference: &str) -> bool {
    SESSION_REFERENCE_ALIASES
        .iter()
        .any(|alias| reference.eq_ignore_ascii_case(alias))
}

fn session_id_from_path(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| {
            name.strip_suffix(&format!(".{PRIMARY_SESSION_EXTENSION}"))
                .or_else(|| name.strip_suffix(&format!(".{LEGACY_SESSION_EXTENSION}")))
        })
        .map(ToOwned::to_owned)
}

fn format_missing_session_reference(reference: &str, sessions_root: &Path) -> String {
    // #80: show the actual workspace-fingerprint directory instead of lying about .claw/sessions/
    let fingerprint_dir = sessions_root
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("<unknown>");
    format!(
        "session not found: {reference}\nHint: managed sessions live in .claw/sessions/{fingerprint_dir}/ (workspace-specific partition).\nTry `{LATEST_SESSION_REFERENCE}` for the most recent session or `/session list` in the REPL."
    )
}

fn format_no_managed_sessions(sessions_root: &Path) -> String {
    // #80: show the actual workspace-fingerprint directory instead of lying about .claw/sessions/
    let fingerprint_dir = sessions_root
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("<unknown>");
    format!(
        "no managed sessions found in .claw/sessions/{fingerprint_dir}/\nStart `claw` to create a session, then rerun with `--resume {LATEST_SESSION_REFERENCE}`.\nNote: /resume {LATEST_SESSION_REFERENCE} searches all workspaces."
    )
}

fn format_legacy_session_missing_workspace_root(
    session_path: &Path,
    workspace_root: &Path,
) -> String {
    format!(
        "legacy session is missing workspace binding: {}\nOpen it from its original workspace or re-save it from {}.",
        session_path.display(),
        workspace_root.display()
    )
}

fn workspace_roots_match(left: &Path, right: &Path) -> bool {
    canonicalize_for_compare(left) == canonicalize_for_compare(right)
}

fn canonicalize_for_compare(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_is_within_workspace(path: &Path, workspace_root: &Path) -> bool {
    canonicalize_for_compare(path).starts_with(canonicalize_for_compare(workspace_root))
}

#[cfg(test)]
mod tests {
    use super::{
        create_managed_session_handle_for, fork_managed_session_for, is_session_reference_alias,
        list_managed_sessions_for, load_managed_session_for, resolve_session_reference_for,
        workspace_fingerprint, ManagedSessionSummary, SessionControlError, SessionStore,
        LATEST_SESSION_REFERENCE,
    };
    use crate::session::Session;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-session-control-{nanos}"))
    }

    fn persist_session(root: &Path, text: &str) -> Session {
        let mut session = Session::new().with_workspace_root(root.to_path_buf());
        session
            .push_user_text(text)
            .expect("session message should save");
        let handle = create_managed_session_handle_for(root, &session.session_id)
            .expect("managed session handle should build");
        let session = session.with_persistence_path(handle.path.clone());
        session
            .save_to_path(&handle.path)
            .expect("session should persist");
        session
    }

    fn wait_for_next_millisecond() {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_millis();
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_millis()
            <= start
        {}
    }

    fn summary_by_id<'a>(
        summaries: &'a [ManagedSessionSummary],
        id: &str,
    ) -> &'a ManagedSessionSummary {
        summaries
            .iter()
            .find(|summary| summary.id == id)
            .expect("session summary should exist")
    }

    #[test]
    fn latest_session_prefers_semantic_updated_at_over_file_mtime() {
        let mut sessions = vec![
            ManagedSessionSummary {
                id: "older-file-newer-session".to_string(),
                path: PathBuf::from("/tmp/older"),
                updated_at_ms: 200,
                modified_epoch_millis: 100,
                message_count: 2,
                parent_session_id: None,
                branch_name: None,
            },
            ManagedSessionSummary {
                id: "newer-file-older-session".to_string(),
                path: PathBuf::from("/tmp/newer"),
                updated_at_ms: 100,
                modified_epoch_millis: 200,
                message_count: 1,
                parent_session_id: None,
                branch_name: None,
            },
        ];

        crate::session_control::sort_managed_sessions(&mut sessions);

        assert_eq!(sessions[0].id, "older-file-newer-session");
        assert_eq!(sessions[1].id, "newer-file-older-session");
    }

    #[test]
    fn creates_and_lists_managed_sessions() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let older = persist_session(&root, "older session");
        wait_for_next_millisecond();
        let newer = persist_session(&root, "newer session");

        // when
        let sessions = list_managed_sessions_for(&root).expect("managed sessions should list");

        // then
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, newer.session_id);
        assert_eq!(summary_by_id(&sessions, &older.session_id).message_count, 1);
        assert_eq!(summary_by_id(&sessions, &newer.session_id).message_count, 1);
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }

    #[test]
    fn resolves_latest_alias_and_loads_session_from_workspace_root() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let older = persist_session(&root, "older session");
        wait_for_next_millisecond();
        let newer = persist_session(&root, "newer session");

        // when
        let handle = resolve_session_reference_for(&root, LATEST_SESSION_REFERENCE)
            .expect("latest alias should resolve");
        let loaded = load_managed_session_for(&root, "recent")
            .expect("recent alias should load the latest session");

        // then
        assert_eq!(handle.id, newer.session_id);
        assert_eq!(loaded.handle.id, newer.session_id);
        assert_eq!(loaded.session.messages.len(), 1);
        assert_ne!(loaded.handle.id, older.session_id);
        assert!(is_session_reference_alias("last"));
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }

    #[test]
    fn forks_session_into_managed_storage_with_lineage() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let source = persist_session(&root, "parent session");

        // when
        let forked = fork_managed_session_for(&root, &source, Some("incident-review".to_string()))
            .expect("session should fork");
        let sessions = list_managed_sessions_for(&root).expect("managed sessions should list");
        let summary = summary_by_id(&sessions, &forked.handle.id);

        // then
        assert_eq!(forked.parent_session_id, source.session_id);
        assert_eq!(forked.branch_name.as_deref(), Some("incident-review"));
        assert_eq!(
            summary.parent_session_id.as_deref(),
            Some(source.session_id.as_str())
        );
        assert_eq!(summary.branch_name.as_deref(), Some("incident-review"));
        assert_eq!(
            forked.session.persistence_path(),
            Some(forked.handle.path.as_path())
        );
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }

    // ------------------------------------------------------------------
    // Per-worktree session isolation (SessionStore) tests
    // ------------------------------------------------------------------

    fn persist_session_via_store(store: &SessionStore, text: &str) -> Session {
        let mut session = Session::new().with_workspace_root(store.workspace_root().to_path_buf());
        session
            .push_user_text(text)
            .expect("session message should save");
        let handle = store.create_handle(&session.session_id);
        let session = session.with_persistence_path(handle.path.clone());
        session
            .save_to_path(&handle.path)
            .expect("session should persist");
        session
    }

    #[test]
    fn workspace_fingerprint_is_deterministic_and_differs_per_path() {
        // given
        let path_a = Path::new("/tmp/worktree-alpha");
        let path_b = Path::new("/tmp/worktree-beta");

        // when
        let fp_a1 = workspace_fingerprint(path_a);
        let fp_a2 = workspace_fingerprint(path_a);
        let fp_b = workspace_fingerprint(path_b);

        // then
        assert_eq!(fp_a1, fp_a2, "same path must produce the same fingerprint");
        assert_ne!(
            fp_a1, fp_b,
            "different paths must produce different fingerprints"
        );
        assert_eq!(fp_a1.len(), 16, "fingerprint must be a 16-char hex string");
    }

    /// #151 regression: equivalent paths (e.g. `/tmp/foo` vs `/private/tmp/foo`
    /// on macOS where `/tmp` is a symlink to `/private/tmp`) must resolve to
    /// the same session store. Previously they diverged because
    /// `workspace_fingerprint()` hashed the raw path string. Now
    /// `SessionStore::from_cwd()` canonicalizes first.
    #[test]
    fn session_store_from_cwd_canonicalizes_equivalent_paths() {
        let base = temp_dir();
        let real_dir = base.join("real-workspace");
        fs::create_dir_all(&real_dir).expect("real workspace should exist");

        // Build two stores via different but equivalent path representations:
        // the raw path and the canonicalized path.
        let raw_path = real_dir.clone();
        let canonical_path = fs::canonicalize(&real_dir).expect("canonicalize ok");

        let store_from_raw =
            SessionStore::from_cwd(&raw_path).expect("store from raw should build");
        let store_from_canonical =
            SessionStore::from_cwd(&canonical_path).expect("store from canonical should build");

        assert_eq!(
            store_from_raw.sessions_dir(),
            store_from_canonical.sessions_dir(),
            "equivalent paths must produce the same sessions dir (raw={} canonical={})",
            raw_path.display(),
            canonical_path.display()
        );

        if base.exists() {
            fs::remove_dir_all(base).expect("cleanup ok");
        }
    }

    #[test]
    fn session_store_from_cwd_isolates_sessions_by_workspace() {
        // given
        let base = temp_dir();
        let workspace_a = base.join("repo-alpha");
        let workspace_b = base.join("repo-beta");
        fs::create_dir_all(&workspace_a).expect("workspace a should exist");
        fs::create_dir_all(&workspace_b).expect("workspace b should exist");

        let store_a = SessionStore::from_cwd(&workspace_a).expect("store a should build");
        let store_b = SessionStore::from_cwd(&workspace_b).expect("store b should build");

        // when
        let session_a = persist_session_via_store(&store_a, "alpha work");
        let _session_b = persist_session_via_store(&store_b, "beta work");

        // then — each store only sees its own sessions
        let list_a = store_a.list_sessions().expect("list a");
        let list_b = store_b.list_sessions().expect("list b");
        assert_eq!(list_a.len(), 1, "store a should see exactly one session");
        assert_eq!(list_b.len(), 1, "store b should see exactly one session");
        assert_eq!(list_a[0].id, session_a.session_id);
        assert_ne!(
            store_a.sessions_dir(),
            store_b.sessions_dir(),
            "session directories must differ across workspaces"
        );
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_from_data_dir_namespaces_by_workspace() {
        // given
        let base = temp_dir();
        let data_dir = base.join("global-data");
        let workspace_a = PathBuf::from("/tmp/project-one");
        let workspace_b = PathBuf::from("/tmp/project-two");
        fs::create_dir_all(&data_dir).expect("data dir should exist");

        let store_a =
            SessionStore::from_data_dir(&data_dir, &workspace_a).expect("store a should build");
        let store_b =
            SessionStore::from_data_dir(&data_dir, &workspace_b).expect("store b should build");

        // when
        persist_session_via_store(&store_a, "work in project-one");
        persist_session_via_store(&store_b, "work in project-two");

        // then
        assert_ne!(
            store_a.sessions_dir(),
            store_b.sessions_dir(),
            "data-dir stores must namespace by workspace"
        );
        assert_eq!(store_a.list_sessions().expect("list a").len(), 1);
        assert_eq!(store_b.list_sessions().expect("list b").len(), 1);
        assert_eq!(store_a.workspace_root(), workspace_a.as_path());
        assert_eq!(store_b.workspace_root(), workspace_b.as_path());
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_create_and_load_round_trip() {
        // given
        let base = temp_dir();
        fs::create_dir_all(&base).expect("base dir should exist");
        let store = SessionStore::from_cwd(&base).expect("store should build");
        let session = persist_session_via_store(&store, "round-trip message");

        // when
        let loaded = store
            .load_session(&session.session_id)
            .expect("session should load via store");

        // then
        assert_eq!(loaded.handle.id, session.session_id);
        assert_eq!(loaded.session.messages.len(), 1);
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_rejects_legacy_session_from_other_workspace() {
        // given
        let base = temp_dir();
        let workspace_a = base.join("repo-alpha");
        let workspace_b = base.join("repo-beta");
        fs::create_dir_all(&workspace_a).expect("workspace a should exist");
        fs::create_dir_all(&workspace_b).expect("workspace b should exist");
        // #151: canonicalize so test expectations match the store's canonical
        // workspace_root. Without this, the test builds sessions with a raw
        // path but the store resolves to the canonical form.
        let workspace_a = fs::canonicalize(&workspace_a).unwrap_or(workspace_a);
        let workspace_b = fs::canonicalize(&workspace_b).unwrap_or(workspace_b);

        let store_b = SessionStore::from_cwd(&workspace_b).expect("store b should build");
        let legacy_root = workspace_b.join(".claw").join("sessions");
        fs::create_dir_all(&legacy_root).expect("legacy root should exist");
        let legacy_path = legacy_root.join("legacy-cross.jsonl");
        let session = Session::new()
            .with_workspace_root(workspace_a.clone())
            .with_persistence_path(legacy_path.clone());
        session
            .save_to_path(&legacy_path)
            .expect("legacy session should persist");

        // when
        let err = store_b
            .load_session("legacy-cross")
            .expect_err("workspace mismatch should be rejected");

        // then
        match err {
            SessionControlError::WorkspaceMismatch { expected, actual } => {
                assert_eq!(expected, workspace_b);
                assert_eq!(actual, workspace_a);
            }
            other => panic!("expected workspace mismatch, got {other:?}"),
        }
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_loads_safe_legacy_session_from_same_workspace() {
        // given
        let base = temp_dir();
        fs::create_dir_all(&base).expect("base dir should exist");
        // #151: canonicalize for path-representation consistency with store.
        let base = fs::canonicalize(&base).unwrap_or(base);
        let store = SessionStore::from_cwd(&base).expect("store should build");
        let legacy_root = base.join(".claw").join("sessions");
        let legacy_path = legacy_root.join("legacy-safe.jsonl");
        fs::create_dir_all(&legacy_root).expect("legacy root should exist");
        let session = Session::new()
            .with_workspace_root(base.clone())
            .with_persistence_path(legacy_path.clone());
        session
            .save_to_path(&legacy_path)
            .expect("legacy session should persist");

        // when
        let loaded = store
            .load_session("legacy-safe")
            .expect("same-workspace legacy session should load");

        // then
        assert_eq!(loaded.handle.id, session.session_id);
        assert_eq!(loaded.handle.path, legacy_path);
        assert_eq!(loaded.session.workspace_root(), Some(base.as_path()));
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_loads_unbound_legacy_session_from_same_workspace() {
        // given
        let base = temp_dir();
        fs::create_dir_all(&base).expect("base dir should exist");
        // #151: canonicalize for path-representation consistency with store.
        let base = fs::canonicalize(&base).unwrap_or(base);
        let store = SessionStore::from_cwd(&base).expect("store should build");
        let legacy_root = base.join(".claw").join("sessions");
        let legacy_path = legacy_root.join("legacy-unbound.json");
        fs::create_dir_all(&legacy_root).expect("legacy root should exist");
        let session = Session::new().with_persistence_path(legacy_path.clone());
        session
            .save_to_path(&legacy_path)
            .expect("legacy session should persist");

        // when
        let loaded = store
            .load_session("legacy-unbound")
            .expect("same-workspace legacy session without workspace binding should load");

        // then
        assert_eq!(loaded.handle.path, legacy_path);
        assert_eq!(loaded.session.workspace_root(), None);
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_latest_and_resolve_reference() {
        // given
        let base = temp_dir();
        fs::create_dir_all(&base).expect("base dir should exist");
        let store = SessionStore::from_cwd(&base).expect("store should build");
        let _older = persist_session_via_store(&store, "older");
        wait_for_next_millisecond();
        let newer = persist_session_via_store(&store, "newer");

        // when
        let latest = store.latest_session().expect("latest should resolve");
        let handle = store
            .resolve_reference("latest")
            .expect("latest alias should resolve");

        // then
        assert_eq!(latest.id, newer.session_id);
        assert_eq!(handle.id, newer.session_id);
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }

    #[test]
    fn session_store_fork_stays_in_same_namespace() {
        // given
        let base = temp_dir();
        fs::create_dir_all(&base).expect("base dir should exist");
        let store = SessionStore::from_cwd(&base).expect("store should build");
        let source = persist_session_via_store(&store, "parent work");

        // when
        let forked = store
            .fork_session(&source, Some("bugfix".to_string()))
            .expect("fork should succeed");
        let sessions = store.list_sessions().expect("list sessions");

        // then
        assert_eq!(
            sessions.len(),
            2,
            "forked session must land in the same namespace"
        );
        assert_eq!(forked.parent_session_id, source.session_id);
        assert_eq!(forked.branch_name.as_deref(), Some("bugfix"));
        assert!(
            forked.handle.path.starts_with(store.sessions_dir()),
            "forked session path must be inside the store namespace"
        );
        fs::remove_dir_all(base).expect("temp dir should clean up");
    }
}
