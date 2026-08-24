use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const TRUST_PROMPT_CUES: &[&str] = &[
    "do you trust the files in this folder",
    "trust the files in this folder",
    "trust this folder",
    "allow and continue",
    "yes, proceed",
];

/// Resolution method for trust decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPolicy {
    /// Automatically trust this path (allowlisted)
    AutoTrust,
    /// Require manual approval
    RequireApproval,
    /// Deny trust for this path
    Deny,
}

/// Events emitted during trust resolution lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrustEvent {
    /// Trust prompt was detected and is required
    TrustRequired {
        /// Current working directory where trust is needed
        cwd: String,
        /// Optional repo identifier
        #[serde(skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        /// Optional worktree path
        #[serde(skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
    },
    /// Trust was resolved (granted)
    TrustResolved {
        /// Current working directory
        cwd: String,
        /// The policy that was applied
        policy: TrustPolicy,
        /// How the trust was resolved
        resolution: TrustResolution,
    },
    /// Trust was denied
    TrustDenied {
        /// Current working directory
        cwd: String,
        /// Reason for denial
        reason: String,
    },
}

/// How trust was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustResolution {
    /// Automatically granted due to allowlist
    AutoAllowlisted,
    /// Manually approved by user
    ManualApproval,
}

/// Entry in the trust allowlist with pattern matching support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAllowlistEntry {
    /// Repository path or glob pattern to match
    pub pattern: String,
    /// Optional worktree subpath pattern
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_pattern: Option<String>,
    /// Human-readable description of why this is allowlisted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl TrustAllowlistEntry {
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            worktree_pattern: None,
            description: None,
        }
    }

    #[must_use]
    pub fn with_worktree_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.worktree_pattern = Some(pattern.into());
        self
    }

    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Configuration for trust resolution with allowlist/denylist support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustConfig {
    /// Allowlisted paths with pattern matching
    pub allowlisted: Vec<TrustAllowlistEntry>,
    /// Denied paths (exact or prefix matches)
    pub denied: Vec<PathBuf>,
    /// Whether to emit events for trust decisions
    #[serde(default = "default_emit_events")]
    pub emit_events: bool,
}

fn default_emit_events() -> bool {
    true
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            allowlisted: Vec::new(),
            denied: Vec::new(),
            emit_events: true,
        }
    }
}

impl TrustConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_allowlisted(mut self, path: impl Into<String>) -> Self {
        self.allowlisted.push(TrustAllowlistEntry::new(path));
        self
    }

    #[must_use]
    pub fn with_allowlisted_entry(mut self, entry: TrustAllowlistEntry) -> Self {
        self.allowlisted.push(entry);
        self
    }

    #[must_use]
    pub fn with_denied(mut self, path: impl Into<PathBuf>) -> Self {
        self.denied.push(path.into());
        self
    }

    /// Check if a path matches an allowlisted entry using glob patterns.
    #[must_use]
    pub fn is_allowlisted(
        &self,
        cwd: &str,
        worktree: Option<&str>,
    ) -> Option<&TrustAllowlistEntry> {
        self.allowlisted.iter().find(|entry| {
            let path_matches = Self::pattern_matches(&entry.pattern, cwd);
            if !path_matches {
                return false;
            }

            match (&entry.worktree_pattern, worktree) {
                (Some(wt_pattern), Some(wt)) => Self::pattern_matches(wt_pattern, wt),
                (Some(_), None) => false,
                (None, _) => true,
            }
        })
    }

    /// Match a pattern against a path string.
    /// Supports exact matching and glob patterns (* and ?).
    fn pattern_matches(pattern: &str, path: &str) -> bool {
        let pattern = pattern.trim();
        let path = path.trim();

        // Exact match
        if pattern == path {
            return true;
        }

        // Normalize paths for comparison
        let pattern_normalized = pattern.replace("//", "/");
        let path_normalized = path.replace("//", "/");

        // Check if pattern is a path prefix (e.g., "/tmp/worktrees" matches "/tmp/worktrees/repo-a")
        // This handles the common case of directory containment
        if !pattern_normalized.contains('*') && !pattern_normalized.contains('?') {
            // Prefix match: pattern is a directory that contains path
            if path_normalized.starts_with(&pattern_normalized) {
                let rest = &path_normalized[pattern_normalized.len()..];
                // Must be exact match or continue with /
                return rest.is_empty() || rest.starts_with('/');
            }
        }

        // Check if pattern ends with wildcard (prefix match)
        if pattern_normalized.ends_with("/*") {
            let prefix = pattern_normalized.trim_end_matches("/*");
            if let Some(rest) = path_normalized.strip_prefix(prefix) {
                // Must either be exact match or continue with /
                return rest.is_empty() || rest.starts_with('/');
            }
        } else if pattern_normalized.ends_with('*') && !pattern_normalized.contains("/*/") {
            // Simple trailing * (not a path component wildcard)
            let prefix = pattern_normalized.trim_end_matches('*');
            if let Some(rest) = path_normalized.strip_prefix(prefix) {
                return rest.is_empty() || !rest.starts_with('/');
            }
        }

        // Check if pattern is a path component match (bounded by /)
        if path_normalized
            .split('/')
            .any(|component| component == pattern_normalized)
        {
            return true;
        }

        // Check if pattern appears as a substring within a path component
        // (e.g., "repo" matches "/tmp/worktrees/repo-a")
        if path_normalized
            .split('/')
            .any(|component| component.contains(&pattern_normalized))
        {
            return true;
        }

        // Glob matching for patterns with ? or * in the middle
        if pattern.contains('?') || pattern.contains("/*/") || pattern.starts_with("*/") {
            return Self::glob_matches(&pattern_normalized, &path_normalized);
        }

        false
    }

    /// Simple glob pattern matching (? matches single char, * matches any sequence).
    /// Handles patterns like /tmp/*/repo-* where * matches path components.
    fn glob_matches(pattern: &str, path: &str) -> bool {
        // Use recursive backtracking for proper glob matching
        Self::glob_match_recursive(pattern, path, 0, 0)
    }

    fn glob_match_recursive(pattern: &str, path: &str, p_idx: usize, s_idx: usize) -> bool {
        let p_chars: Vec<char> = pattern.chars().collect();
        let s_chars: Vec<char> = path.chars().collect();

        let mut p = p_idx;
        let mut s = s_idx;

        while p < p_chars.len() {
            match p_chars[p] {
                '*' => {
                    // Try all possible matches for *
                    p += 1;
                    if p >= p_chars.len() {
                        // * at end matches everything remaining
                        return true;
                    }
                    // Try matching 0 or more characters
                    for skip in 0..=(s_chars.len() - s) {
                        if Self::glob_match_recursive(pattern, path, p, s + skip) {
                            return true;
                        }
                    }
                    return false;
                }
                '?' => {
                    // ? matches exactly one character
                    if s >= s_chars.len() {
                        return false;
                    }
                    p += 1;
                    s += 1;
                }
                c => {
                    // Exact character match
                    if s >= s_chars.len() || s_chars[s] != c {
                        return false;
                    }
                    p += 1;
                    s += 1;
                }
            }
        }

        // Pattern exhausted - path must also be exhausted
        s >= s_chars.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustDecision {
    NotRequired,
    Required {
        policy: TrustPolicy,
        events: Vec<TrustEvent>,
    },
}

impl TrustDecision {
    #[must_use]
    pub fn policy(&self) -> Option<TrustPolicy> {
        match self {
            Self::NotRequired => None,
            Self::Required { policy, .. } => Some(*policy),
        }
    }

    #[must_use]
    pub fn events(&self) -> &[TrustEvent] {
        match self {
            Self::NotRequired => &[],
            Self::Required { events, .. } => events,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrustResolver {
    config: TrustConfig,
}

impl TrustResolver {
    #[must_use]
    pub fn new(config: TrustConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn resolve(&self, cwd: &str, worktree: Option<&str>, screen_text: &str) -> TrustDecision {
        if !detect_trust_prompt(screen_text) {
            return TrustDecision::NotRequired;
        }

        let repo = extract_repo_name(cwd);
        let mut events = vec![TrustEvent::TrustRequired {
            cwd: cwd.to_owned(),
            repo: repo.clone(),
            worktree: worktree.map(String::from),
        }];

        // Check denylist first
        if let Some(matched_root) = self
            .config
            .denied
            .iter()
            .find(|root| path_matches(cwd, root))
        {
            let reason = format!("cwd matches denied trust root: {}", matched_root.display());
            events.push(TrustEvent::TrustDenied {
                cwd: cwd.to_owned(),
                reason,
            });
            return TrustDecision::Required {
                policy: TrustPolicy::Deny,
                events,
            };
        }

        // Check allowlist with pattern matching
        if self.config.is_allowlisted(cwd, worktree).is_some() {
            events.push(TrustEvent::TrustResolved {
                cwd: cwd.to_owned(),
                policy: TrustPolicy::AutoTrust,
                resolution: TrustResolution::AutoAllowlisted,
            });
            return TrustDecision::Required {
                policy: TrustPolicy::AutoTrust,
                events,
            };
        }

        // Check for manual trust resolution via screen text analysis
        if detect_manual_approval(screen_text) {
            events.push(TrustEvent::TrustResolved {
                cwd: cwd.to_owned(),
                policy: TrustPolicy::RequireApproval,
                resolution: TrustResolution::ManualApproval,
            });
            return TrustDecision::Required {
                policy: TrustPolicy::RequireApproval,
                events,
            };
        }

        TrustDecision::Required {
            policy: TrustPolicy::RequireApproval,
            events,
        }
    }

    #[must_use]
    pub fn trusts(&self, cwd: &str, worktree: Option<&str>) -> bool {
        // Check denylist first
        let denied = self
            .config
            .denied
            .iter()
            .any(|root| path_matches(cwd, root));

        if denied {
            return false;
        }

        // Check allowlist using pattern matching
        self.config.is_allowlisted(cwd, worktree).is_some()
    }
}

#[must_use]
pub fn detect_trust_prompt(screen_text: &str) -> bool {
    let lowered = screen_text.to_ascii_lowercase();
    TRUST_PROMPT_CUES
        .iter()
        .any(|needle| lowered.contains(needle))
}

#[must_use]
pub fn path_matches_trusted_root(cwd: &str, trusted_root: &str) -> bool {
    path_matches(cwd, &normalize_path(Path::new(trusted_root)))
}

fn path_matches(candidate: &str, root: &Path) -> bool {
    let candidate = normalize_path(Path::new(candidate));
    let root = normalize_path(root);
    candidate == root || candidate.starts_with(&root)
}

fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Extract repository name from a path for event context.
fn extract_repo_name(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    // Ask git from the cwd itself. Walking ancestors manually can accidentally
    // classify synthetic/nonexistent paths as an unrelated parent repo (for
    // example `/tmp/.git`), which makes trust events point at the wrong repo.
    if path.is_dir() {
        if let Ok(output) = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .output()
        {
            if output.status.success() {
                let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !root.is_empty() {
                    return Path::new(&root)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string());
                }
            }
        }
    }
    // Fallback: use the last component of the path
    path.file_name().map(|n| n.to_string_lossy().to_string())
}

/// Detect if the screen text indicates manual approval was granted.
fn detect_manual_approval(screen_text: &str) -> bool {
    let lowered = screen_text.to_ascii_lowercase();
    // Look for indicators that user manually approved
    MANUAL_APPROVAL_CUES.iter().any(|cue| lowered.contains(cue))
}

const MANUAL_APPROVAL_CUES: &[&str] = &[
    "yes, i trust",
    "i trust this",
    "trusted manually",
    "approval granted",
];

#[cfg(test)]
mod path_matching_tests {
    use super::*;

    #[test]
    fn glob_pattern_star_matches_any_sequence() {
        assert!(TrustConfig::pattern_matches("/tmp/*", "/tmp/foo"));
        assert!(TrustConfig::pattern_matches("/tmp/*", "/tmp/bar/baz"));
        assert!(!TrustConfig::pattern_matches("/tmp/*", "/other/tmp/foo"));
    }

    #[test]
    fn glob_pattern_question_matches_single_char() {
        assert!(TrustConfig::pattern_matches("/tmp/test?", "/tmp/test1"));
        assert!(TrustConfig::pattern_matches("/tmp/test?", "/tmp/testA"));
        assert!(!TrustConfig::pattern_matches("/tmp/test?", "/tmp/test12"));
        assert!(!TrustConfig::pattern_matches("/tmp/test?", "/tmp/test"));
    }

    #[test]
    fn pattern_matches_exact() {
        assert!(TrustConfig::pattern_matches(
            "/tmp/worktrees",
            "/tmp/worktrees"
        ));
        assert!(!TrustConfig::pattern_matches(
            "/tmp/worktrees",
            "/tmp/worktrees-other"
        ));
    }

    #[test]
    fn pattern_matches_prefix_with_wildcard() {
        assert!(TrustConfig::pattern_matches(
            "/tmp/worktrees/*",
            "/tmp/worktrees/repo-a"
        ));
        assert!(TrustConfig::pattern_matches(
            "/tmp/worktrees/*",
            "/tmp/worktrees/repo-a/subdir"
        ));
        assert!(!TrustConfig::pattern_matches(
            "/tmp/worktrees/*",
            "/tmp/other/repo"
        ));
    }

    #[test]
    fn pattern_matches_contains() {
        // Pattern contained within path
        assert!(TrustConfig::pattern_matches(
            "worktrees",
            "/tmp/worktrees/repo-a"
        ));
        assert!(TrustConfig::pattern_matches(
            "repo",
            "/tmp/worktrees/repo-a"
        ));
    }

    #[test]
    fn allowlist_entry_with_worktree_pattern() {
        let config = TrustConfig::new().with_allowlisted_entry(
            TrustAllowlistEntry::new("/tmp/worktrees/*")
                .with_worktree_pattern("*/.git")
                .with_description("Git worktrees"),
        );

        // Should match when both patterns match
        assert!(config
            .is_allowlisted("/tmp/worktrees/repo-a", Some("/tmp/worktrees/repo-a/.git"))
            .is_some());

        // Should not match when worktree pattern doesn't match
        assert!(config
            .is_allowlisted("/tmp/worktrees/repo-a", Some("/other/path"))
            .is_none());

        // Should not match when a worktree pattern is required but no worktree is supplied
        assert!(config
            .is_allowlisted("/tmp/worktrees/repo-a", None)
            .is_none());

        // Should match when no worktree pattern required and path matches
        let config_no_worktree = TrustConfig::new().with_allowlisted("/tmp/worktrees/*");
        assert!(config_no_worktree
            .is_allowlisted("/tmp/worktrees/repo-a", None)
            .is_some());
    }

    #[test]
    fn allowlist_entry_returns_matched_entry() {
        let entry = TrustAllowlistEntry::new("/tmp/worktrees/*").with_description("Test worktrees");
        let config = TrustConfig::new().with_allowlisted_entry(entry.clone());

        let matched = config.is_allowlisted("/tmp/worktrees/repo-a", None);
        assert!(matched.is_some());
        assert_eq!(
            matched.unwrap().description,
            Some("Test worktrees".to_string())
        );
    }

    #[test]
    fn complex_glob_patterns() {
        // Multiple wildcards
        assert!(TrustConfig::pattern_matches(
            "/tmp/*/repo-*",
            "/tmp/worktrees/repo-123"
        ));
        assert!(TrustConfig::pattern_matches(
            "/tmp/*/repo-*",
            "/tmp/other/repo-abc"
        ));
        assert!(!TrustConfig::pattern_matches(
            "/tmp/*/repo-*",
            "/tmp/worktrees/other"
        ));

        // Mixed ? and *
        assert!(TrustConfig::pattern_matches(
            "/tmp/test?/*.txt",
            "/tmp/test1/file.txt"
        ));
        assert!(TrustConfig::pattern_matches(
            "/tmp/test?/*.txt",
            "/tmp/testA/subdir/file.txt"
        ));
    }

    #[test]
    fn serde_serialization_roundtrip() {
        let config = TrustConfig::new()
            .with_allowlisted_entry(
                TrustAllowlistEntry::new("/tmp/worktrees/*")
                    .with_worktree_pattern("*/.git")
                    .with_description("Git worktrees"),
            )
            .with_denied("/tmp/malicious");

        let json = serde_json::to_string(&config).expect("serialization failed");
        let deserialized: TrustConfig =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(config.allowlisted.len(), deserialized.allowlisted.len());
        assert_eq!(config.denied.len(), deserialized.denied.len());
        assert_eq!(config.emit_events, deserialized.emit_events);
    }

    #[test]
    fn trust_event_serialization() {
        let event = TrustEvent::TrustRequired {
            cwd: "/tmp/test".to_string(),
            repo: Some("test-repo".to_string()),
            worktree: Some("/tmp/test/.git".to_string()),
        };

        let json = serde_json::to_string(&event).expect("serialization failed");
        assert!(json.contains("trust_required"));
        assert!(json.contains("/tmp/test"));
        assert!(json.contains("test-repo"));

        let deserialized: TrustEvent = serde_json::from_str(&json).expect("deserialization failed");
        match deserialized {
            TrustEvent::TrustRequired {
                cwd,
                repo,
                worktree,
            } => {
                assert_eq!(cwd, "/tmp/test");
                assert_eq!(repo, Some("test-repo".to_string()));
                assert_eq!(worktree, Some("/tmp/test/.git".to_string()));
            }
            _ => panic!("wrong event type"),
        }
    }

    #[test]
    fn trust_event_resolved_serialization() {
        let event = TrustEvent::TrustResolved {
            cwd: "/tmp/test".to_string(),
            policy: TrustPolicy::AutoTrust,
            resolution: TrustResolution::AutoAllowlisted,
        };

        let json = serde_json::to_string(&event).expect("serialization failed");
        assert!(json.contains("trust_resolved"));
        assert!(json.contains("auto_allowlisted"));

        let deserialized: TrustEvent = serde_json::from_str(&json).expect("deserialization failed");
        match deserialized {
            TrustEvent::TrustResolved { resolution, .. } => {
                assert_eq!(resolution, TrustResolution::AutoAllowlisted);
            }
            _ => panic!("wrong event type"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_manual_approval, detect_trust_prompt, path_matches_trusted_root,
        TrustAllowlistEntry, TrustConfig, TrustDecision, TrustEvent, TrustPolicy, TrustResolution,
        TrustResolver,
    };

    #[test]
    fn detects_known_trust_prompt_copy() {
        // given
        let screen_text = "Do you trust the files in this folder?\n1. Yes, proceed\n2. No";

        // when
        let detected = detect_trust_prompt(screen_text);

        // then
        assert!(detected);
    }

    #[test]
    fn does_not_emit_events_when_prompt_is_absent() {
        // given
        let resolver = TrustResolver::new(TrustConfig::new().with_allowlisted("/tmp/worktrees"));

        // when
        let decision = resolver.resolve("/tmp/worktrees/repo-a", None, "Ready for your input\n>");

        // then
        assert_eq!(decision, TrustDecision::NotRequired);
        assert_eq!(decision.events(), &[]);
        assert_eq!(decision.policy(), None);
    }

    #[test]
    fn auto_trusts_allowlisted_cwd_after_prompt_detection() {
        // given
        let resolver = TrustResolver::new(TrustConfig::new().with_allowlisted("/tmp/worktrees"));

        // when
        let decision = resolver.resolve(
            "/tmp/worktrees/repo-a",
            None,
            "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
        );

        // then
        assert_eq!(decision.policy(), Some(TrustPolicy::AutoTrust));
        let events = decision.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], TrustEvent::TrustRequired { .. }));
        assert!(matches!(
            events[1],
            TrustEvent::TrustResolved {
                policy: TrustPolicy::AutoTrust,
                resolution: TrustResolution::AutoAllowlisted,
                ..
            }
        ));
    }

    #[test]
    fn requires_approval_for_unknown_cwd_after_prompt_detection() {
        // given
        let resolver = TrustResolver::new(TrustConfig::new().with_allowlisted("/tmp/worktrees"));

        // when
        let decision = resolver.resolve(
            "/tmp/other/repo-b",
            None,
            "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
        );

        // then
        assert_eq!(decision.policy(), Some(TrustPolicy::RequireApproval));
        assert_eq!(
            decision.events(),
            &[TrustEvent::TrustRequired {
                cwd: "/tmp/other/repo-b".to_string(),
                repo: Some("repo-b".to_string()),
                worktree: None,
            }]
        );
    }

    #[test]
    fn denied_root_takes_precedence_over_allowlist() {
        // given
        let resolver = TrustResolver::new(
            TrustConfig::new()
                .with_allowlisted("/tmp/worktrees")
                .with_denied("/tmp/worktrees/repo-c"),
        );

        // when
        let decision = resolver.resolve(
            "/tmp/worktrees/repo-c",
            None,
            "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
        );

        // then
        assert_eq!(decision.policy(), Some(TrustPolicy::Deny));
        assert_eq!(
            decision.events(),
            &[
                TrustEvent::TrustRequired {
                    cwd: "/tmp/worktrees/repo-c".to_string(),
                    repo: Some("repo-c".to_string()),
                    worktree: None,
                },
                TrustEvent::TrustDenied {
                    cwd: "/tmp/worktrees/repo-c".to_string(),
                    reason: "cwd matches denied trust root: /tmp/worktrees/repo-c".to_string(),
                },
            ]
        );
    }

    #[test]
    fn auto_trusts_with_glob_pattern_allowlist() {
        // given
        let resolver = TrustResolver::new(TrustConfig::new().with_allowlisted("/tmp/worktrees/*"));

        // when - any repo under /tmp/worktrees should auto-trust
        let decision = resolver.resolve(
            "/tmp/worktrees/repo-a",
            None,
            "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
        );

        // then
        assert_eq!(decision.policy(), Some(TrustPolicy::AutoTrust));
    }

    #[test]
    fn resolve_with_worktree_pattern_matching() {
        // given
        let config = TrustConfig::new().with_allowlisted_entry(
            TrustAllowlistEntry::new("/tmp/worktrees/*").with_worktree_pattern("*/.git"),
        );
        let resolver = TrustResolver::new(config);

        // when - with worktree that matches the pattern
        let decision = resolver.resolve(
            "/tmp/worktrees/repo-a",
            Some("/tmp/worktrees/repo-a/.git"),
            "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
        );

        // then - should auto-trust because both patterns match
        assert_eq!(decision.policy(), Some(TrustPolicy::AutoTrust));
    }

    #[test]
    fn manual_approval_detected_from_screen_text() {
        // given
        let resolver = TrustResolver::new(TrustConfig::new());

        // when - screen text indicates manual approval
        let decision = resolver.resolve(
            "/tmp/some/repo",
            None,
            "Do you trust the files in this folder?\nUser selected: Yes, I trust this folder",
        );

        // then - should detect manual approval
        assert_eq!(decision.policy(), Some(TrustPolicy::RequireApproval));
        let events = decision.events();
        assert!(events.len() >= 2);
        assert!(matches!(
            events[events.len() - 1],
            TrustEvent::TrustResolved {
                resolution: TrustResolution::ManualApproval,
                ..
            }
        ));
    }

    #[test]
    fn sibling_prefix_does_not_match_trusted_root() {
        // given
        let trusted_root = "/tmp/worktrees";
        let sibling_path = "/tmp/worktrees-other/repo-d";

        // when
        let matched = path_matches_trusted_root(sibling_path, trusted_root);

        // then
        assert!(!matched);
    }

    #[test]
    fn detects_manual_approval_cues() {
        assert!(detect_manual_approval(
            "User selected: Yes, I trust this folder"
        ));
        assert!(detect_manual_approval(
            "I trust this repository and its contents"
        ));
        assert!(detect_manual_approval("Approval granted by user"));
        assert!(!detect_manual_approval(
            "Do you trust the files in this folder?"
        ));
        assert!(!detect_manual_approval("Some unrelated text"));
    }

    #[test]
    fn trust_config_default_emit_events() {
        let config = TrustConfig::default();
        assert!(config.emit_events);
    }

    #[test]
    fn trust_resolver_trusts_method() {
        let resolver = TrustResolver::new(
            TrustConfig::new()
                .with_allowlisted("/tmp/worktrees/*")
                .with_denied("/tmp/worktrees/bad-repo"),
        );

        // Should trust allowlisted paths
        assert!(resolver.trusts("/tmp/worktrees/good-repo", None));

        // Should not trust denied paths
        assert!(!resolver.trusts("/tmp/worktrees/bad-repo", None));

        // Should not trust unknown paths
        assert!(!resolver.trusts("/tmp/other/repo", None));
    }

    #[test]
    fn trust_policy_serde_roundtrip() {
        for policy in [
            TrustPolicy::AutoTrust,
            TrustPolicy::RequireApproval,
            TrustPolicy::Deny,
        ] {
            let json = serde_json::to_string(&policy).expect("serialization failed");
            let deserialized: TrustPolicy =
                serde_json::from_str(&json).expect("deserialization failed");
            assert_eq!(policy, deserialized);
        }
    }

    #[test]
    fn trust_resolution_serde_roundtrip() {
        for resolution in [
            TrustResolution::AutoAllowlisted,
            TrustResolution::ManualApproval,
        ] {
            let json = serde_json::to_string(&resolution).expect("serialization failed");
            let deserialized: TrustResolution =
                serde_json::from_str(&json).expect("deserialization failed");
            assert_eq!(resolution, deserialized);
        }
    }
}
