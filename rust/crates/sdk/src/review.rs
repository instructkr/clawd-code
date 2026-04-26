//! Review workflow for agent changes.
//!
//! Provides structured diff generation, risk classification, approval/rejection
//! gates, batch review, and review history — all designed for humans to
//! efficiently review agent-produced work.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Risk levels
// ---------------------------------------------------------------------------

/// Risk classification for a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Low-risk: documentation, comments, formatting, tests.
    Low,
    /// Medium-risk: logic changes, dependency updates, config changes.
    Medium,
    /// High-risk: security-sensitive code, auth, credentials, data migration.
    High,
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskLevel::Low => write!(f, "low"),
            RiskLevel::Medium => write!(f, "medium"),
            RiskLevel::High => write!(f, "high"),
        }
    }
}

// ---------------------------------------------------------------------------
// Change record
// ---------------------------------------------------------------------------

/// A single change made by an agent, ready for review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeRecord {
    /// Unique identifier for this change.
    pub id: String,
    /// The agent turn that produced this change.
    pub turn_id: String,
    /// Session ID.
    pub session_id: String,
    /// Human-readable summary of the change.
    pub summary: String,
    /// Files affected by this change.
    pub files: Vec<FileChange>,
    /// Risk classification.
    pub risk: RiskLevel,
    /// Timestamp (ms since epoch).
    pub timestamp_ms: u64,
}

/// A file-level change within a change record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    /// File path (relative to workspace root).
    pub path: String,
    /// Type of change.
    pub change_type: FileChangeType,
    /// Number of lines added.
    pub lines_added: usize,
    /// Number of lines removed.
    pub lines_removed: usize,
    /// Optional unified diff snippet.
    pub diff_hunk: Option<String>,
}

/// Type of file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed,
}

impl fmt::Display for FileChangeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileChangeType::Created => write!(f, "created"),
            FileChangeType::Modified => write!(f, "modified"),
            FileChangeType::Deleted => write!(f, "deleted"),
            FileChangeType::Renamed => write!(f, "renamed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Review decisions
// ---------------------------------------------------------------------------

/// A decision made on a change record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewDecision {
    /// The change being reviewed.
    pub change_id: String,
    /// The decision.
    pub decision: Decision,
    /// Optional reason/comment.
    pub comment: Option<String>,
    /// Reviewer identity (human name, "auto-approve", etc).
    pub reviewer: String,
    /// Timestamp (ms since epoch).
    pub timestamp_ms: u64,
}

/// The outcome of a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Approved,
    Rejected,
    ChangesRequested,
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decision::Approved => write!(f, "approved"),
            Decision::Rejected => write!(f, "rejected"),
            Decision::ChangesRequested => write!(f, "changes_requested"),
        }
    }
}

// ---------------------------------------------------------------------------
// Review gate
// ---------------------------------------------------------------------------

/// A configurable gate that determines when an agent must pause for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewGate {
    /// Name of this gate.
    pub name: String,
    /// Minimum risk level that triggers this gate. Changes at or above
    /// this level require review before the agent can proceed.
    pub min_risk: RiskLevel,
    /// File patterns that always trigger this gate (glob patterns).
    pub sensitive_paths: Vec<String>,
    /// Whether this gate is enabled.
    pub enabled: bool,
}

impl ReviewGate {
    /// Create a new review gate.
    #[must_use]
    pub fn new(name: &str, min_risk: RiskLevel) -> Self {
        Self {
            name: name.to_string(),
            min_risk,
            sensitive_paths: Vec::new(),
            enabled: true,
        }
    }

    /// Check if a change requires review through this gate.
    #[must_use]
    pub fn requires_review(&self, change: &ChangeRecord) -> bool {
        if !self.enabled {
            return false;
        }

        // Check risk level
        if change.risk >= self.min_risk {
            return true;
        }

        // Check sensitive file patterns
        for pattern in &self.sensitive_paths {
            for file in &change.files {
                if glob_matches(pattern, &file.path) {
                    return true;
                }
            }
        }

        false
    }

    /// Add a sensitive file pattern.
    #[must_use]
    pub fn sensitive_path(mut self, pattern: &str) -> Self {
        self.sensitive_paths.push(pattern.to_string());
        self
    }
}

// ---------------------------------------------------------------------------
// Risk classifier
// ---------------------------------------------------------------------------

/// Classifies the risk of changes based on file paths and change types.
pub struct RiskClassifier {
    /// File paths that are always high-risk.
    high_risk_paths: Vec<String>,
    /// File paths that are medium-risk.
    medium_risk_paths: Vec<String>,
}

impl RiskClassifier {
    /// Create a new classifier with default rules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            high_risk_paths: vec![
                "credentials".to_string(),
                "secrets".to_string(),
                ".env".to_string(),
                "auth".to_string(),
                "oauth".to_string(),
                "security".to_string(),
                "crypto".to_string(),
                "migration".to_string(),
            ],
            medium_risk_paths: vec![
                "Cargo.toml".to_string(),
                "package.json".to_string(),
                "config".to_string(),
                "settings".to_string(),
                "Dockerfile".to_string(),
                "Containerfile".to_string(),
            ],
        }
    }

    /// Classify the risk of a change record.
    #[must_use]
    pub fn classify(&self, change: &ChangeRecord) -> RiskLevel {
        let mut max_risk = RiskLevel::Low;

        for file in &change.files {
            let file_risk = self.classify_file(file);
            if file_risk > max_risk {
                max_risk = file_risk;
            }
        }

        // Deletions are inherently riskier
        if change.files.iter().any(|f| f.change_type == FileChangeType::Deleted) {
            max_risk = max_risk.max(RiskLevel::Medium);
        }

        max_risk
    }

    /// Classify the risk of a single file change.
    #[must_use]
    pub fn classify_file(&self, file: &FileChange) -> RiskLevel {
        let path_lower = file.path.to_lowercase();

        for pattern in &self.high_risk_paths {
            if path_lower.contains(pattern.to_lowercase().as_str()) {
                return RiskLevel::High;
            }
        }

        for pattern in &self.medium_risk_paths {
            if path_lower.contains(pattern.to_lowercase().as_str()) {
                return RiskLevel::Medium;
            }
        }

        // Large changes are medium-risk
        if file.lines_added + file.lines_removed > 100 {
            return RiskLevel::Medium;
        }

        RiskLevel::Low
    }

    /// Add a high-risk file path pattern.
    pub fn add_high_risk(&mut self, pattern: &str) {
        self.high_risk_paths.push(pattern.to_string());
    }

    /// Add a medium-risk file path pattern.
    pub fn add_medium_risk(&mut self, pattern: &str) {
        self.medium_risk_paths.push(pattern.to_string());
    }
}

impl Default for RiskClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Review manager
// ---------------------------------------------------------------------------

/// Manages the review lifecycle: submitting changes, checking gates,
/// recording decisions, and maintaining review history.
pub struct ReviewManager {
    /// Pending changes awaiting review.
    pending: BTreeMap<String, ChangeRecord>,
    /// Review decisions (change_id -> decision).
    decisions: BTreeMap<String, ReviewDecision>,
    /// Active review gates.
    gates: Vec<ReviewGate>,
    /// Risk classifier.
    classifier: RiskClassifier,
    /// Counter for generating IDs.
    next_id: u64,
}

impl ReviewManager {
    /// Create a new review manager with default gates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            decisions: BTreeMap::new(),
            gates: vec![
                ReviewGate::new("high-risk", RiskLevel::High)
                    .sensitive_path("**/credentials*")
                    .sensitive_path("**/.env*")
                    .sensitive_path("**/secrets/**"),
                ReviewGate::new("security-sensitive", RiskLevel::Medium)
                    .sensitive_path("**/auth/**")
                    .sensitive_path("**/security/**")
                    .sensitive_path("**/crypto/**"),
            ],
            classifier: RiskClassifier::new(),
            next_id: 1,
        }
    }

    /// Submit a change for review. Returns the change ID.
    /// The change's risk level is automatically classified.
    pub fn submit(&mut self, mut change: ChangeRecord) -> Result<String, String> {
        if change.id.is_empty() {
            change.id = format!("chg-{}", self.next_id);
            self.next_id += 1;
        }

        // Auto-classify risk
        change.risk = self.classifier.classify(&change);

        let id = change.id.clone();
        self.pending.insert(id.clone(), change);
        Ok(id)
    }

    /// Check if a change requires review through any gate.
    #[must_use]
    pub fn requires_review(&self, change_id: &str) -> bool {
        match self.pending.get(change_id) {
            Some(change) => self.gates.iter().any(|gate| gate.requires_review(change)),
            None => false,
        }
    }

    /// Get all pending changes that require review.
    #[must_use]
    pub fn pending_reviews(&self) -> Vec<&ChangeRecord> {
        self.pending
            .values()
            .filter(|change| self.gates.iter().any(|gate| gate.requires_review(change)))
            .collect()
    }

    /// Get all pending changes regardless of gate status.
    #[must_use]
    pub fn all_pending(&self) -> Vec<&ChangeRecord> {
        self.pending.values().collect()
    }

    /// Approve a change.
    pub fn approve(&mut self, change_id: &str, reviewer: &str, comment: Option<String>) -> Result<(), String> {
        if !self.pending.contains_key(change_id) {
            return Err(format!("change '{change_id}' not found in pending reviews"));
        }

        let decision = ReviewDecision {
            change_id: change_id.to_string(),
            decision: Decision::Approved,
            comment,
            reviewer: reviewer.to_string(),
            timestamp_ms: now_ms(),
        };

        self.pending.remove(change_id);
        self.decisions.insert(change_id.to_string(), decision);
        Ok(())
    }

    /// Reject a change.
    pub fn reject(&mut self, change_id: &str, reviewer: &str, comment: Option<String>) -> Result<(), String> {
        if !self.pending.contains_key(change_id) {
            return Err(format!("change '{change_id}' not found in pending reviews"));
        }

        let decision = ReviewDecision {
            change_id: change_id.to_string(),
            decision: Decision::Rejected,
            comment,
            reviewer: reviewer.to_string(),
            timestamp_ms: now_ms(),
        };

        self.pending.remove(change_id);
        self.decisions.insert(change_id.to_string(), decision);
        Ok(())
    }

    /// Request changes (neither approve nor reject — agent should revise).
    pub fn request_changes(
        &mut self,
        change_id: &str,
        reviewer: &str,
        comment: Option<String>,
    ) -> Result<(), String> {
        if !self.pending.contains_key(change_id) {
            return Err(format!("change '{change_id}' not found in pending reviews"));
        }

        // The change stays in pending for resubmission
        let decision = ReviewDecision {
            change_id: change_id.to_string(),
            decision: Decision::ChangesRequested,
            comment,
            reviewer: reviewer.to_string(),
            timestamp_ms: now_ms(),
        };

        self.decisions.insert(format!("{}-req-{}", change_id, now_ms()), decision);
        Ok(())
    }

    /// Batch-approve all pending changes at or below the given risk level.
    /// Returns the number of changes approved.
    pub fn batch_approve(&mut self, max_risk: RiskLevel, reviewer: &str) -> usize {
        let ids: Vec<String> = self
            .pending
            .values()
            .filter(|c| c.risk <= max_risk)
            .map(|c| c.id.clone())
            .collect();

        let count = ids.len();
        for id in ids {
            let _ = self.approve(&id, reviewer, Some("batch approval".to_string()));
        }
        count
    }

    /// Get the review history (all decisions).
    #[must_use]
    pub fn history(&self) -> Vec<&ReviewDecision> {
        self.decisions.values().collect()
    }

    /// Get the decision for a specific change.
    #[must_use]
    pub fn decision(&self, change_id: &str) -> Option<&ReviewDecision> {
        self.decisions.get(change_id)
    }

    /// Get a pending change by ID.
    #[must_use]
    pub fn get_pending(&self, change_id: &str) -> Option<&ChangeRecord> {
        self.pending.get(change_id)
    }

    /// Add a review gate.
    pub fn add_gate(&mut self, gate: ReviewGate) {
        self.gates.push(gate);
    }

    /// Get the risk classifier (read-only).
    #[must_use]
    pub fn classifier(&self) -> &RiskClassifier {
        &self.classifier
    }
}

impl Default for ReviewManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
}

/// Simple glob matching. Supports `*` (any chars) and `**` (any path segments).
fn glob_matches(pattern: &str, path: &str) -> bool {
    // Fast path: no **, just delegate to simple glob
    if !pattern.contains("**") {
        return simple_glob_match(pattern, path);
    }

    // Split on ** — each segment must appear in order
    let parts: Vec<&str> = pattern.split("**").collect();
    if parts.len() == 2 && parts[0].is_empty() && parts[1].is_empty() {
        return true; // "**" matches everything
    }

    let mut search_from = 0;
    for (i, part) in parts.iter().enumerate() {
        let trimmed = part.trim_matches('/');
        if trimmed.is_empty() {
            continue;
        }

        if i == parts.len() - 1 {
            // Last segment: must match at the end using simple glob
            // Find all possible positions for the prefix before any *
            let star_pos = trimmed.find('*');
            let prefix = match star_pos {
                Some(0) => "", // starts with *, just match end
                Some(p) => &trimmed[..p],
                None => trimmed,
            };

            if prefix.is_empty() {
                // Match anywhere at the end
                return simple_glob_match(trimmed, &path[search_from..]);
            }

            // Find the last occurrence of prefix
            match path[search_from..].rfind(prefix) {
                Some(pos) => {
                    let full_match_start = search_from + pos;
                    return simple_glob_match(trimmed, &path[full_match_start..]);
                }
                None => return false,
            }
        }

        // Non-last segment: find the prefix (before any *) in remaining path
        let star_pos = trimmed.find('*');
        let prefix = match star_pos {
            Some(0) => "",
            Some(p) => &trimmed[..p],
            None => trimmed,
        };

        if prefix.is_empty() {
            continue; // Wildcard at start, just move on
        }

        match path[search_from..].find(prefix) {
            Some(pos) => {
                search_from = search_from + pos + prefix.len();
            }
            None => return false,
        }
    }
    true
}

fn simple_glob_match(pattern: &str, path: &str) -> bool {
    let pat_chars: Vec<char> = pattern.chars().collect();
    let path_chars: Vec<char> = path.chars().collect();
    glob_match_recursive(&pat_chars, &path_chars, 0, 0)
}

fn glob_match_recursive(pat: &[char], path: &[char], pi: usize, xi: usize) -> bool {
    if pi == pat.len() && xi == path.len() {
        return true;
    }
    if pi == pat.len() {
        return false;
    }
    match pat[pi] {
        '*' => {
            // Try matching 0 or more characters
            for end in xi..=path.len() {
                if glob_match_recursive(pat, path, pi + 1, end) {
                    return true;
                }
            }
            false
        }
        '?' if xi < path.len() => glob_match_recursive(pat, path, pi + 1, xi + 1),
        c if xi < path.len() && path[xi] == c => glob_match_recursive(pat, path, pi + 1, xi + 1),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- RiskLevel ---

    #[test]
    fn risk_level_ordering() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
        assert_eq!(RiskLevel::Low.to_string(), "low");
        assert_eq!(RiskLevel::Medium.to_string(), "medium");
        assert_eq!(RiskLevel::High.to_string(), "high");
    }

    // --- RiskClassifier ---

    #[test]
    fn classify_credentials_as_high_risk() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "config/credentials.json".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 5,
            lines_removed: 2,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::High);
    }

    #[test]
    fn classify_cargo_toml_as_medium_risk() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "Cargo.toml".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 3,
            lines_removed: 1,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::Medium);
    }

    #[test]
    fn classify_readme_as_low_risk() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "README.md".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 10,
            lines_removed: 5,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::Low);
    }

    #[test]
    fn classify_large_change_as_medium() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "src/lib.rs".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 80,
            lines_removed: 40,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::Medium);
    }

    #[test]
    fn classify_deletion_as_medium() {
        let classifier = RiskClassifier::new();
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "Deleted a file".to_string(),
            files: vec![FileChange {
                path: "old_file.txt".to_string(),
                change_type: FileChangeType::Deleted,
                lines_added: 0,
                lines_removed: 20,
                diff_hunk: None,
            }],
            risk: RiskLevel::Low, // will be reclassified
            timestamp_ms: 0,
        };
        assert_eq!(classifier.classify(&change), RiskLevel::Medium);
    }

    #[test]
    fn custom_high_risk_pattern() {
        let mut classifier = RiskClassifier::new();
        classifier.add_high_risk("billing");
        let file = FileChange {
            path: "src/billing/mod.rs".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 5,
            lines_removed: 2,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::High);
    }

    // --- ReviewGate ---

    #[test]
    fn gate_triggers_on_risk_level() {
        let gate = ReviewGate::new("test", RiskLevel::Medium);
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "High risk change".to_string(),
            files: vec![FileChange {
                path: "secrets/key.pem".to_string(),
                change_type: FileChangeType::Modified,
                lines_added: 1,
                lines_removed: 1,
                diff_hunk: None,
            }],
            risk: RiskLevel::High,
            timestamp_ms: 0,
        };
        assert!(gate.requires_review(&change));
    }

    #[test]
    fn gate_does_not_trigger_below_risk() {
        let gate = ReviewGate::new("test", RiskLevel::High);
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "Low risk".to_string(),
            files: vec![FileChange {
                path: "README.md".to_string(),
                change_type: FileChangeType::Modified,
                lines_added: 5,
                lines_removed: 2,
                diff_hunk: None,
            }],
            risk: RiskLevel::Low,
            timestamp_ms: 0,
        };
        assert!(!gate.requires_review(&change));
    }

    #[test]
    fn gate_triggers_on_sensitive_path() {
        let gate = ReviewGate::new("test", RiskLevel::High)
            .sensitive_path("**/credentials*");
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "Touched credentials".to_string(),
            files: vec![FileChange {
                path: "config/credentials.json".to_string(),
                change_type: FileChangeType::Modified,
                lines_added: 1,
                lines_removed: 1,
                diff_hunk: None,
            }],
            risk: RiskLevel::Low,
            timestamp_ms: 0,
        };
        assert!(gate.requires_review(&change));
    }

    #[test]
    fn disabled_gate_never_triggers() {
        let mut gate = ReviewGate::new("test", RiskLevel::Low);
        gate.enabled = false;
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "anything".to_string(),
            files: vec![],
            risk: RiskLevel::High,
            timestamp_ms: 0,
        };
        assert!(!gate.requires_review(&change));
    }

    // --- ReviewManager ---

    fn make_change(summary: &str, path: &str, change_type: FileChangeType) -> ChangeRecord {
        ChangeRecord {
            id: String::new(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: summary.to_string(),
            files: vec![FileChange {
                path: path.to_string(),
                change_type,
                lines_added: 10,
                lines_removed: 5,
                diff_hunk: None,
            }],
            risk: RiskLevel::Low, // reclassified on submit
            timestamp_ms: now_ms(),
        }
    }

    #[test]
    fn submit_auto_classifies_risk() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("Tweak auth", "src/auth/mod.rs", FileChangeType::Modified))
            .expect("submit");
        let change = mgr.get_pending(&id).expect("should exist");
        assert_eq!(change.risk, RiskLevel::High);
    }

    #[test]
    fn approve_removes_from_pending() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("Docs", "README.md", FileChangeType::Modified))
            .expect("submit");

        mgr.approve(&id, "human", Some("looks good".to_string()))
            .expect("approve");

        assert!(mgr.get_pending(&id).is_none());
        let decision = mgr.decision(&id).expect("should have decision");
        assert_eq!(decision.decision, Decision::Approved);
        assert_eq!(decision.reviewer, "human");
    }

    #[test]
    fn reject_removes_from_pending() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("Bad change", "src/main.rs", FileChangeType::Modified))
            .expect("submit");

        mgr.reject(&id, "human", Some("wrong approach".to_string()))
            .expect("reject");

        assert!(mgr.get_pending(&id).is_none());
        assert_eq!(mgr.decision(&id).unwrap().decision, Decision::Rejected);
    }

    #[test]
    fn request_changes_keeps_in_pending() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("Needs work", "src/lib.rs", FileChangeType::Modified))
            .expect("submit");

        mgr.request_changes(&id, "human", Some("fix the tests".to_string()))
            .expect("request changes");

        // Still pending
        assert!(mgr.get_pending(&id).is_some());
    }

    #[test]
    fn approve_nonexistent_fails() {
        let mut mgr = ReviewManager::new();
        let result = mgr.approve("nonexistent", "human", None);
        assert!(result.is_err());
    }

    #[test]
    fn pending_reviews_filters_by_gates() {
        let mut mgr = ReviewManager::new();
        let low_id = mgr.submit(make_change("Docs", "README.md", FileChangeType::Modified))
            .expect("submit");
        let high_id = mgr.submit(make_change("Auth", "src/auth/mod.rs", FileChangeType::Modified))
            .expect("submit");

        let pending = mgr.pending_reviews();
        let pending_ids: Vec<&str> = pending.iter().map(|c| c.id.as_str()).collect();

        // The high-risk auth change should require review, the low-risk docs shouldn't
        assert!(pending_ids.contains(&high_id.as_str()));
        assert!(!pending_ids.contains(&low_id.as_str()));
    }

    #[test]
    fn batch_approve_by_risk_level() {
        let mut mgr = ReviewManager::new();
        mgr.submit(make_change("Docs", "README.md", FileChangeType::Modified))
            .expect("submit");
        mgr.submit(make_change("Config", "Cargo.toml", FileChangeType::Modified))
            .expect("submit");
        mgr.submit(make_change("Auth", "src/auth/mod.rs", FileChangeType::Modified))
            .expect("submit");

        let approved = mgr.batch_approve(RiskLevel::Medium, "batch-bot");
        // README (Low) and Cargo.toml (Medium) should be approved, auth (High) should not
        assert_eq!(approved, 2);
        assert_eq!(mgr.all_pending().len(), 1); // Only auth remains
    }

    #[test]
    fn review_history_tracks_all_decisions() {
        let mut mgr = ReviewManager::new();
        let id1 = mgr.submit(make_change("A", "a.txt", FileChangeType::Created)).expect("submit");
        let id2 = mgr.submit(make_change("B", "b.txt", FileChangeType::Created)).expect("submit");

        mgr.approve(&id1, "alice", None).expect("approve");
        mgr.reject(&id2, "bob", Some("nope".to_string())).expect("reject");

        let history = mgr.history();
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn requires_review_checks_gates() {
        let mut mgr = ReviewManager::new();
        let low_id = mgr.submit(make_change("Docs", "README.md", FileChangeType::Modified))
            .expect("submit");
        let high_id = mgr.submit(make_change("Secrets", ".env", FileChangeType::Modified))
            .expect("submit");

        assert!(!mgr.requires_review(&low_id));
        assert!(mgr.requires_review(&high_id));
    }

    // --- Glob matching ---

    #[test]
    fn glob_star_matches_any_chars() {
        assert!(simple_glob_match("*.txt", "readme.txt"));
        assert!(!simple_glob_match("*.txt", "readme.md"));
    }

    #[test]
    fn glob_double_star_matches_path_segments() {
        assert!(glob_matches("**/credentials*", "config/credentials.json"));
        assert!(glob_matches("**/credentials*", "credentials.json"));
        assert!(!glob_matches("**/credentials*", "config/settings.json"));
        assert!(glob_matches("**/.env*", ".env.production"));
        assert!(glob_matches("**/secrets/**", "secrets/key.pem"));
    }

    // --- Serde round-trip ---

    #[test]
    fn serde_round_trip_change_record() {
        let record = ChangeRecord {
            id: "chg-1".to_string(),
            turn_id: "turn-1".to_string(),
            session_id: "sess-1".to_string(),
            summary: "Added auth module".to_string(),
            files: vec![FileChange {
                path: "src/auth/mod.rs".to_string(),
                change_type: FileChangeType::Created,
                lines_added: 50,
                lines_removed: 0,
                diff_hunk: Some("+pub mod auth;".to_string()),
            }],
            risk: RiskLevel::High,
            timestamp_ms: 1234567890,
        };

        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: ChangeRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, record);
    }

    #[test]
    fn serde_round_trip_review_decision() {
        let decision = ReviewDecision {
            change_id: "chg-1".to_string(),
            decision: Decision::Approved,
            comment: Some("Looks good".to_string()),
            reviewer: "alice".to_string(),
            timestamp_ms: 1234567890,
        };

        let json = serde_json::to_string(&decision).expect("serialize");
        let parsed: ReviewDecision = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, decision);
    }

    // --- FileChangeType display ---

    #[test]
    fn file_change_type_display() {
        assert_eq!(FileChangeType::Created.to_string(), "created");
        assert_eq!(FileChangeType::Modified.to_string(), "modified");
        assert_eq!(FileChangeType::Deleted.to_string(), "deleted");
        assert_eq!(FileChangeType::Renamed.to_string(), "renamed");
    }

    // --- RiskClassifier edge cases ---

    #[test]
    fn classify_empty_file_list_returns_low() {
        let classifier = RiskClassifier::new();
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "Metadata only".to_string(),
            files: vec![],
            risk: RiskLevel::Low,
            timestamp_ms: 0,
        };
        assert_eq!(classifier.classify(&change), RiskLevel::Low);
    }

    #[test]
    fn classify_env_variants_as_high() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: ".env.production".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 3,
            lines_removed: 1,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::High);
    }

    #[test]
    fn classify_environment_rs_as_low_not_high() {
        // "environment.rs" contains "env" substring but is not a credentials file
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "src/environment.rs".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 5,
            lines_removed: 2,
            diff_hunk: None,
        };
        // ".env" is in high_risk_paths; "src/environment.rs" does NOT contain ".env"
        // but it does contain "env" which is a substring of ".env"...
        // Let's check: path_lower = "src/environment.rs", high_risk = ".env"
        // contains(".env") => false (it's "environment", not ".env")
        assert_eq!(classifier.classify_file(&file), RiskLevel::Low);
    }

    #[test]
    fn classify_case_insensitive_paths() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "SECRETS/key.PEM".to_string(),
            change_type: FileChangeType::Modified,
            lines_added: 1,
            lines_removed: 1,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::High);
    }

    #[test]
    fn classify_renamed_file() {
        let classifier = RiskClassifier::new();
        let file = FileChange {
            path: "src/auth/mod.rs".to_string(),
            change_type: FileChangeType::Renamed,
            lines_added: 0,
            lines_removed: 0,
            diff_hunk: None,
        };
        assert_eq!(classifier.classify_file(&file), RiskLevel::High);
    }

    // --- ReviewManager edge cases ---

    #[test]
    fn submit_generates_unique_sequential_ids() {
        let mut mgr = ReviewManager::new();
        let id1 = mgr.submit(make_change("A", "a.txt", FileChangeType::Created)).expect("submit 1");
        let id2 = mgr.submit(make_change("B", "b.txt", FileChangeType::Created)).expect("submit 2");
        let id3 = mgr.submit(make_change("C", "c.txt", FileChangeType::Created)).expect("submit 3");
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert!(id1.starts_with("chg-"));
    }

    #[test]
    fn approve_same_change_twice_fails() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("A", "a.txt", FileChangeType::Created)).expect("submit");
        mgr.approve(&id, "alice", None).expect("first approve");
        let result = mgr.approve(&id, "alice", None);
        assert!(result.is_err());
    }

    #[test]
    fn reject_nonexistent_fails() {
        let mut mgr = ReviewManager::new();
        let result = mgr.reject("nonexistent", "bot", None);
        assert!(result.is_err());
    }

    #[test]
    fn request_changes_nonexistent_fails() {
        let mut mgr = ReviewManager::new();
        let result = mgr.request_changes("nonexistent", "bot", None);
        assert!(result.is_err());
    }

    #[test]
    fn request_changes_then_approve_accumulates_history() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("A", "a.txt", FileChangeType::Modified)).expect("submit");

        mgr.request_changes(&id, "alice", Some("fix tests".to_string())).expect("request");
        mgr.approve(&id, "alice", Some("fixed".to_string())).expect("approve");

        let history = mgr.history();
        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|d| d.decision == Decision::ChangesRequested));
        assert!(history.iter().any(|d| d.decision == Decision::Approved));
    }

    #[test]
    fn submit_duplicate_explicit_id_overwrites() {
        let mut mgr = ReviewManager::new();
        let mut change1 = make_change("First", "a.txt", FileChangeType::Created);
        change1.id = "chg-x".to_string();
        mgr.submit(change1).expect("submit 1");

        let mut change2 = make_change("Second", "b.txt", FileChangeType::Created);
        change2.id = "chg-x".to_string();
        mgr.submit(change2).expect("submit 2 (overwrites)");

        // Only one pending change with id "chg-x"
        assert_eq!(mgr.all_pending().len(), 1);
        assert_eq!(mgr.get_pending("chg-x").unwrap().summary, "Second");
    }

    #[test]
    fn all_pending_includes_low_risk_changes() {
        let mut mgr = ReviewManager::new();
        let id = mgr.submit(make_change("Docs", "README.md", FileChangeType::Modified)).expect("submit");

        assert_eq!(mgr.all_pending().len(), 1);
        assert!(mgr.all_pending()[0].id == id);
        assert!(mgr.pending_reviews().is_empty()); // Low risk doesn't trigger default gates
    }

    #[test]
    fn batch_approve_empty_pending_returns_zero() {
        let mut mgr = ReviewManager::new();
        let count = mgr.batch_approve(RiskLevel::High, "bot");
        assert_eq!(count, 0);
    }

    #[test]
    fn batch_approve_high_risk_with_low_max_approves_nothing() {
        let mut mgr = ReviewManager::new();
        mgr.submit(make_change("Auth", "src/auth/mod.rs", FileChangeType::Modified)).expect("submit");
        mgr.submit(make_change("Secrets", ".env", FileChangeType::Modified)).expect("submit");

        let count = mgr.batch_approve(RiskLevel::Low, "bot");
        assert_eq!(count, 0);
        assert_eq!(mgr.all_pending().len(), 2);
    }

    // --- ReviewGate with custom gates ---

    #[test]
    fn custom_low_risk_gate_with_sensitive_path() {
        let mut mgr = ReviewManager::new();
        mgr.add_gate(ReviewGate::new("certs", RiskLevel::Low).sensitive_path("**/*.pem"));

        let change = make_change("Cert", "certs/server.pem", FileChangeType::Modified);
        let id = mgr.submit(change).expect("submit");

        // Low risk but matches *.pem sensitive path
        assert!(mgr.requires_review(&id));
    }

    #[test]
    fn gate_sensitive_path_no_match_does_not_trigger() {
        // Gate only triggers for *.pem files; a .rs file shouldn't trigger it
        let gate = ReviewGate::new("certs", RiskLevel::High).sensitive_path("**/*.pem");
        let change = ChangeRecord {
            id: "c1".to_string(),
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            summary: "Rust file".to_string(),
            files: vec![FileChange {
                path: "src/main.rs".to_string(),
                change_type: FileChangeType::Modified,
                lines_added: 5,
                lines_removed: 2,
                diff_hunk: None,
            }],
            risk: RiskLevel::Low,
            timestamp_ms: 0,
        };
        assert!(!gate.requires_review(&change));
    }

    // --- Glob matching edge cases ---

    #[test]
    fn glob_question_mark_matches_exactly_one_char() {
        assert!(simple_glob_match("file?.txt", "file1.txt"));
        assert!(!simple_glob_match("file?.txt", "file.txt"));
        assert!(!simple_glob_match("file?.txt", "file12.txt"));
    }

    #[test]
    fn glob_empty_patterns() {
        assert!(simple_glob_match("", ""));
        assert!(!simple_glob_match("", "foo"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "foo"));
    }

    #[test]
    fn glob_exact_match_no_wildcards() {
        assert!(simple_glob_match("README.md", "README.md"));
        assert!(!simple_glob_match("README.md", "readme.md"));
    }

    #[test]
    fn glob_case_sensitive() {
        assert!(!glob_matches("**/SECRETS/**", "secrets/key.pem"));
    }

    #[test]
    fn glob_multiple_double_star_segments() {
        assert!(glob_matches("**/src/**/*.rs", "project/src/module/file.rs"));
        assert!(!glob_matches("**/src/**/*.rs", "project/lib/file.rs"));
    }

    // --- Serde round-trips ---

    #[test]
    fn serde_risk_level_round_trip() {
        for level in [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High] {
            let json = serde_json::to_string(&level).expect("serialize");
            let parsed: RiskLevel = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, level);
        }
        // Default serde serialization uses PascalCase
        assert_eq!(serde_json::to_string(&RiskLevel::High).unwrap(), "\"High\"");
    }

    #[test]
    fn serde_decision_round_trip() {
        for decision in [Decision::Approved, Decision::Rejected, Decision::ChangesRequested] {
            let json = serde_json::to_string(&decision).expect("serialize");
            let parsed: Decision = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, decision);
        }
    }

    #[test]
    fn serde_file_change_type_round_trip() {
        for ct in [FileChangeType::Created, FileChangeType::Modified, FileChangeType::Deleted, FileChangeType::Renamed] {
            let json = serde_json::to_string(&ct).expect("serialize");
            let parsed: FileChangeType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, ct);
        }
    }

    #[test]
    fn serde_review_gate_round_trip() {
        let gate = ReviewGate {
            name: "security".to_string(),
            min_risk: RiskLevel::High,
            sensitive_paths: vec!["**/*.pem".to_string(), "**/secrets/**".to_string()],
            enabled: true,
        };
        let json = serde_json::to_string(&gate).expect("serialize");
        let parsed: ReviewGate = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.name, gate.name);
        assert_eq!(parsed.min_risk, gate.min_risk);
        assert_eq!(parsed.sensitive_paths, gate.sensitive_paths);
        assert_eq!(parsed.enabled, gate.enabled);
    }

    // --- Decision display ---

    #[test]
    fn decision_display() {
        assert_eq!(Decision::Approved.to_string(), "approved");
        assert_eq!(Decision::Rejected.to_string(), "rejected");
        assert_eq!(Decision::ChangesRequested.to_string(), "changes_requested");
    }
}
