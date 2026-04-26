use std::path::{Path, PathBuf};

use runtime::Session;

/// Configuration for session storage.
#[derive(Debug, Clone, Default)]
pub struct SessionManagerConfig {
    /// Directory where session files are stored.
    pub session_dir: Option<PathBuf>,
    /// Whether to store sessions (vs. in-memory only).
    pub persist: bool,
}

/// Manages session creation, loading, and listing.
///
/// This is the SDK equivalent of `runtime::SessionStore` with a simpler API.
#[derive(Debug, Clone)]
pub struct SessionManager {
    config: SessionManagerConfig,
}

impl SessionManager {
    /// Create a new session manager with the given config.
    #[must_use]
    pub fn new(config: SessionManagerConfig) -> Self {
        Self { config }
    }

    /// Create a session manager that stores sessions in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(SessionManagerConfig {
            persist: false,
            ..Default::default()
        })
    }

    /// Create a session manager that persists sessions to the given directory.
    #[must_use]
    pub fn persisted(session_dir: PathBuf) -> Self {
        Self::new(SessionManagerConfig {
            session_dir: Some(session_dir),
            persist: true,
        })
    }

    /// Create a new empty session.
    #[must_use]
    pub fn create_session(&self) -> Session {
        Session::new()
    }

    /// List all session files in the configured session directory.
    pub fn list_sessions(&self) -> Result<Vec<PathBuf>, String> {
        let Some(dir) = &self.config.session_dir else {
            return Ok(Vec::new());
        };
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("failed to read session dir: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read entry: {e}))"))?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext == "jsonl" || ext == "json")
            {
                sessions.push(path);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    /// Save a session to its configured path.
    pub fn save_session(&self, session: &Session, path: &Path) -> Result<(), String> {
        if !self.config.persist {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("failed to create session dir: {e}"))?;
        }
        session.save_to_path(path).map_err(|e| e.to_string())
    }

    /// Load a session from a file path.
    pub fn load_session(path: &Path) -> Result<Session, String> {
        Session::load_from_path(path).map_err(|e| e.to_string())
    }

    /// Delete a session file.
    pub fn delete_session(path: &Path) -> Result<(), String> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| format!("failed to delete session: {e}"))
        } else {
            Err(format!("session not found: {}", path.display()))
        }
    }

    /// Check if a session file exists.
    #[must_use]
    pub fn session_exists(path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn in_memory_manager_creates_session() {
        let manager = SessionManager::in_memory();
        let session = manager.create_session();
        assert!(!session.session_id.is_empty());
    }

    #[test]
    fn persisted_manager_lists_sessions() {
        let dir = std::env::temp_dir().join(format!("sdk-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");

        let manager = SessionManager::persisted(dir.clone());

        // Initially empty
        assert!(manager.list_sessions().unwrap().is_empty());

        // Create a session file
        let session = manager.create_session();
        let path = dir.join("test-session.jsonl");
        session.save_to_path(&path).expect("save session");

        let sessions = manager.list_sessions().unwrap();
        assert!(!sessions.is_empty());
        assert!(sessions.iter().any(|p| p.ends_with("test-session.jsonl")));

        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn delete_session_removes_file() {
        let dir = std::env::temp_dir().join(format!("sdk-test-del-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");

        let session = Session::new();
        let path = dir.join("to-delete.jsonl");
        session.save_to_path(&path).expect("save session");
        assert!(SessionManager::session_exists(&path));

        SessionManager::delete_session(&path).expect("delete session");
        assert!(!SessionManager::session_exists(&path));

        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn delete_nonexistent_session_returns_error() {
        let path = Path::new("/tmp/__nonexistent_session__");
        let result = SessionManager::delete_session(path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn load_session_from_file() {
        let dir = std::env::temp_dir().join(format!("sdk-test-load-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");

        let mut session = Session::new();
        session
            .push_user_text("hello".to_string())
            .expect("push message");
        let path = dir.join("load-test.jsonl");
        session.save_to_path(&path).expect("save session");

        let loaded = SessionManager::load_session(&path).expect("load session");
        assert_eq!(loaded.messages.len(), 1);

        fs::remove_dir_all(dir).expect("cleanup");
    }
}
