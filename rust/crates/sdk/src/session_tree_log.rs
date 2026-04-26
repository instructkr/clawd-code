//! Session tree persistence using append-only JSONL files.
//!
//! Each line in a `.jsonl` session file is a typed entry that describes one
//! event in the session's history. The tree is reconstructed by replaying
//! entries in order.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::session_tree::SessionTree;

// ---------------------------------------------------------------------------
// JSONL entry types
// ---------------------------------------------------------------------------

/// A single entry in the session tree JSONL log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum TreeEntry {
    /// A user or assistant message at a given tree position.
    #[serde(rename = "message")]
    Message {
        node_id: String,
        parent_id: Option<String>,
        role: String,
        content: String,
        label: Option<String>,
    },

    /// A compaction event that summarized older messages.
    #[serde(rename = "compaction")]
    Compaction {
        node_id: String,
        parent_id: Option<String>,
        summary: String,
        removed_count: usize,
    },

    /// A branch point where the conversation forked.
    #[serde(rename = "branch")]
    Branch {
        branch_id: String,
        from_node_id: String,
        label: Option<String>,
    },

    /// A model change mid-session.
    #[serde(rename = "model_change")]
    ModelChange {
        node_id: String,
        previous: String,
        current: String,
    },

    /// A thinking-level change (e.g. from "none" to "extended").
    #[serde(rename = "thinking_level")]
    ThinkingLevel {
        node_id: String,
        level: String,
    },

    /// A custom/key-value entry for extensions.
    #[serde(rename = "custom")]
    Custom {
        node_id: Option<String>,
        key: String,
        value: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// JSONL persistence
// ---------------------------------------------------------------------------

/// Persistent session tree that writes entries to a JSONL file and can
/// reconstruct the tree from the log.
pub struct SessionTreeLog {
    path: std::path::PathBuf,
    entries: Vec<TreeEntry>,
    tree: SessionTree,
    /// Labels for branch points.
    branch_labels: BTreeMap<String, String>,
}

impl SessionTreeLog {
    /// Create a new session tree log backed by the given file path.
    /// If the file exists, entries are loaded and the tree is reconstructed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let mut log = Self {
            path,
            entries: Vec::new(),
            tree: SessionTree::new(),
            branch_labels: BTreeMap::new(),
        };

        if log.path.exists() {
            log.load_from_file()?;
        }

        Ok(log)
    }

    /// Create a new empty session tree log (does not read from disk).
    #[must_use]
    pub fn new_in_memory() -> Self {
        Self {
            path: std::path::PathBuf::new(),
            entries: Vec::new(),
            tree: SessionTree::new(),
            branch_labels: BTreeMap::new(),
        }
    }

    /// Append an entry and persist it to disk (if backed by a file).
    pub fn append(&mut self, entry: TreeEntry) -> Result<(), String> {
        self.apply_entry(&entry)?;
        self.entries.push(entry.clone());
        self.flush_entry(&entry)
    }

    /// Get the reconstructed in-memory tree.
    #[must_use]
    pub fn tree(&self) -> &SessionTree {
        &self.tree
    }

    /// Get the reconstructed in-memory tree (mutable).
    pub fn tree_mut(&mut self) -> &mut SessionTree {
        &mut self.tree
    }

    /// Get all entries.
    #[must_use]
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Get the branch label for a branch point.
    #[must_use]
    pub fn branch_label(&self, branch_id: &str) -> Option<&str> {
        self.branch_labels.get(branch_id).map(String::as_str)
    }

    /// Get the file path, if backed by a file.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        if self.path.as_os_str().is_empty() {
            None
        } else {
            Some(&self.path)
        }
    }

    /// Walk the tree from root to the active leaf and collect all message
    /// entries along that path. This builds the provider context for an
    /// API call.
    #[must_use]
    pub fn build_session_context(&self) -> Vec<&TreeEntry> {
        let path_ids: Vec<String> = self
            .tree
            .active_path()
            .iter()
            .map(|n| n.id.clone())
            .collect();

        self.entries
            .iter()
            .filter(|e| match e {
                TreeEntry::Message { node_id, .. } => path_ids.contains(node_id),
                TreeEntry::Compaction { node_id, .. } => path_ids.contains(node_id),
                TreeEntry::ModelChange { node_id, .. } => path_ids.contains(node_id),
                TreeEntry::ThinkingLevel { node_id, .. } => path_ids.contains(node_id),
                _ => false,
            })
            .collect()
    }

    /// Fork the tree at the given node and create a new independent session
    /// file from that point. Returns the new `SessionTreeLog` rooted at the
    /// fork point.
    pub fn fork_to_new_file(
        &self,
        node_id: &str,
        new_path: impl AsRef<Path>,
    ) -> Result<SessionTreeLog, String> {
        let path_ids = self.collect_ancestor_ids(node_id);

        let filtered: Vec<TreeEntry> = self
            .entries
            .iter()
            .filter(|e| match e {
                TreeEntry::Message { node_id: nid, .. } => path_ids.contains(nid),
                TreeEntry::Compaction { node_id: nid, .. } => path_ids.contains(nid),
                TreeEntry::ModelChange { node_id: nid, .. } => path_ids.contains(nid),
                TreeEntry::ThinkingLevel { node_id: nid, .. } => path_ids.contains(nid),
                TreeEntry::Branch {
                    from_node_id, ..
                } => path_ids.contains(from_node_id),
                TreeEntry::Custom { node_id: nid, .. } => {
                    nid.as_ref().map_or(false, |id| path_ids.contains(id))
                }
            })
            .cloned()
            .collect();

        let mut new_log = SessionTreeLog::open(new_path)?;
        for entry in &filtered {
            new_log.append(entry.clone())?;
        }
        Ok(new_log)
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn apply_entry(&mut self, entry: &TreeEntry) -> Result<(), String> {
        match entry {
            TreeEntry::Message {
                node_id,
                parent_id,
                role,
                label,
                ..
            } => match parent_id {
                Some(pid) => {
                    self.tree.add_child(node_id, pid, role, label.clone())?;
                }
                None => {
                    self.tree.set_root(node_id, role, label.clone());
                }
            },
            TreeEntry::Compaction {
                node_id,
                parent_id,
                ..
            } => match parent_id {
                Some(pid) => {
                    self.tree.add_child(
                        node_id,
                        pid,
                        "compaction",
                        Some("compaction".to_string()),
                    )?;
                }
                None => {
                    self.tree
                        .set_root(node_id, "compaction", Some("compaction".to_string()));
                }
            },
            TreeEntry::Branch {
                branch_id,
                from_node_id,
                label,
            } => {
                self.tree.fork_at(from_node_id, branch_id)?;
                if let Some(lbl) = label {
                    self.branch_labels
                        .insert(branch_id.clone(), lbl.clone());
                }
            }
            TreeEntry::ModelChange { node_id, .. } => {
                let active = self.tree.active_id().map(String::from).ok_or_else(|| {
                    "cannot apply ModelChange: no active node in tree (append a Message first)"
                        .to_string()
                })?;
                self.tree.add_child(
                    node_id,
                    &active,
                    "system",
                    Some("model_change".to_string()),
                )?;
            }
            TreeEntry::ThinkingLevel { node_id, .. } => {
                let active = self.tree.active_id().map(String::from).ok_or_else(|| {
                    "cannot apply ThinkingLevel: no active node in tree (append a Message first)"
                        .to_string()
                })?;
                self.tree.add_child(
                    node_id,
                    &active,
                    "system",
                    Some("thinking_level".to_string()),
                )?;
            }
            TreeEntry::Custom { .. } => {
                // Custom entries are metadata only, no tree node
            }
        }
        Ok(())
    }

    fn flush_entry(&self, entry: &TreeEntry) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let mut file =
            fs::OpenOptions::new().append(true).create(true).open(&self.path).map_err(
                |e| format!("failed to open session log {:?}: {e}", self.path),
            )?;
        let mut line = serde_json::to_string(entry)
            .map_err(|e| format!("failed to serialize entry: {e}"))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(|e| format!("failed to write entry: {e}"))?;
        Ok(())
    }

    fn load_from_file(&mut self) -> Result<(), String> {
        let file = fs::File::open(&self.path)
            .map_err(|e| format!("failed to open session log {:?}: {e}", self.path))?;
        let reader = std::io::BufReader::new(file);
        let lines: Vec<String> = reader
            .lines()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("failed to read session log {:?}: {e}", self.path))?;

        self.entries.clear();
        self.tree = SessionTree::new();
        self.branch_labels.clear();

        let total = lines.len();
        for (i, line) in lines.into_iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: TreeEntry = match serde_json::from_str(trimmed) {
                Ok(e) => e,
                Err(e) => {
                    // If this is the last line, it may be a truncated write from a crash.
                    // Skip it rather than failing the entire session load.
                    if i == total - 1 {
                        continue;
                    }
                    return Err(format!(
                        "failed to parse entry at line {}: {e}\n  line: {trimmed}",
                        i + 1
                    ));
                }
            };
            self.apply_entry(&entry)?;
            self.entries.push(entry);
        }

        Ok(())
    }

    /// Collect all ancestor node IDs from root to the given node.
    fn collect_ancestor_ids(&self, node_id: &str) -> Vec<String> {
        let mut ids = vec![node_id.to_string()];
        let mut current = self.tree.get(node_id);
        while let Some(node) = current {
            if let Some(pid) = &node.parent_id {
                ids.push(pid.clone());
                current = self.tree.get(pid);
            } else {
                break;
            }
        }
        ids.reverse();
        ids
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_message_entries_and_reconstruct_tree() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Hello".to_string(),
            label: Some("greeting".to_string()),
        })
        .expect("append r1");

        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Hi there!".to_string(),
            label: None,
        })
        .expect("append c1");

        let tree = log.tree();
        assert_eq!(tree.root().unwrap().id, "r1");
        assert_eq!(tree.active().unwrap().id, "c1");
        assert_eq!(tree.active_path().len(), 2);
    }

    #[test]
    fn branch_entry_forks_tree_and_stores_label() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Start".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Response".to_string(),
            label: None,
        })
        .expect("append c1");

        log.append(TreeEntry::Branch {
            branch_id: "b1".to_string(),
            from_node_id: "c1".to_string(),
            label: Some("try-different-approach".to_string()),
        })
        .expect("branch");

        assert_eq!(log.branch_label("b1"), Some("try-different-approach"));
        assert!(log.tree().get("b1").is_some());
    }

    #[test]
    fn compaction_entry_creates_compaction_node() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Long conversation...".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::Compaction {
            node_id: "comp1".to_string(),
            parent_id: Some("r1".to_string()),
            summary: "Summarized earlier messages".to_string(),
            removed_count: 10,
        })
        .expect("append compaction");

        let active = log.tree().active().unwrap();
        assert_eq!(active.id, "comp1");
        assert_eq!(active.role, "compaction");
    }

    #[test]
    fn model_change_entry_creates_system_node() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Hello".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::ModelChange {
            node_id: "mc1".to_string(),
            previous: "claude-sonnet-4-6".to_string(),
            current: "gpt-4o".to_string(),
        })
        .expect("append model change");

        let active = log.tree().active().unwrap();
        assert_eq!(active.id, "mc1");
        assert_eq!(active.role, "system");
    }

    #[test]
    fn custom_entry_does_not_create_tree_node() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Hello".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::Custom {
            node_id: Some("r1".to_string()),
            key: "extension_data".to_string(),
            value: json!({"foo": "bar"}),
        })
        .expect("append custom");

        // Custom entries don't create tree nodes — only root exists
        assert_eq!(log.tree().active().unwrap().id, "r1");
        assert_eq!(log.entries().len(), 2);
    }

    #[test]
    fn build_session_context_returns_active_path_entries() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Hello".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Hi!".to_string(),
            label: None,
        })
        .expect("append c1");

        log.append(TreeEntry::Custom {
            node_id: Some("r1".to_string()),
            key: "ext".to_string(),
            value: json!("data"),
        })
        .expect("append custom");

        let ctx = log.build_session_context();
        assert_eq!(ctx.len(), 2); // r1 and c1 messages, custom is excluded
    }

    #[test]
    fn round_trip_to_file_and_back() {
        let dir = std::env::temp_dir().join("claw_test_session_tree_log");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test_session.jsonl");
        let _ = fs::remove_file(&path); // Clean slate

        // Write entries
        {
            let mut log = SessionTreeLog::open(&path).expect("open for write");
            log.append(TreeEntry::Message {
                node_id: "r1".to_string(),
                parent_id: None,
                role: "user".to_string(),
                content: "Hello".to_string(),
                label: Some("greeting".to_string()),
            })
            .expect("append r1");
            log.append(TreeEntry::Message {
                node_id: "c1".to_string(),
                parent_id: Some("r1".to_string()),
                role: "assistant".to_string(),
                content: "World!".to_string(),
                label: None,
            })
            .expect("append c1");
        }

        // Read back
        let log = SessionTreeLog::open(&path).expect("open for read");
        assert_eq!(log.entries().len(), 2);
        assert_eq!(log.tree().root().unwrap().id, "r1");
        assert_eq!(log.tree().active().unwrap().id, "c1");
        assert_eq!(log.path(), Some(path.as_path()));

        // Verify file content is valid JSONL
        let content = fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let _: TreeEntry = serde_json::from_str(line).expect("valid JSONL entry");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_to_new_file_creates_subset() {
        let dir = std::env::temp_dir().join("claw_test_fork");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("original.jsonl");
        let new_path = dir.join("forked.jsonl");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&new_path);

        let mut log = SessionTreeLog::open(&path).expect("open");
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Start".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Response 1".to_string(),
            label: None,
        })
        .expect("c1");
        log.append(TreeEntry::Message {
            node_id: "c2".to_string(),
            parent_id: Some("c1".to_string()),
            role: "user".to_string(),
            content: "Follow up".to_string(),
            label: None,
        })
        .expect("c2");

        // Fork at c1 — should include r1 and c1, but not c2
        let forked = log.fork_to_new_file("c1", &new_path).expect("fork");
        assert_eq!(forked.entries().len(), 2);
        assert_eq!(forked.tree().active().unwrap().id, "c1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn thinking_level_entry() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Think harder".to_string(),
            label: None,
        })
        .expect("append r1");

        log.append(TreeEntry::ThinkingLevel {
            node_id: "tl1".to_string(),
            level: "extended".to_string(),
        })
        .expect("append thinking level");

        assert_eq!(log.tree().active().unwrap().id, "tl1");
    }

    #[test]
    fn open_missing_file_creates_empty_log() {
        let dir = std::env::temp_dir().join("claw_test_missing");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("nonexistent.jsonl");
        let _ = fs::remove_file(&path);

        let log = SessionTreeLog::open(&path).expect("open missing");
        assert_eq!(log.entries().len(), 0);
        assert!(log.tree().root().is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    // --- Serde round-trip for all variants ---

    #[test]
    fn serde_round_trip_all_variants() {
        let entries = vec![
            TreeEntry::Message {
                node_id: "m1".to_string(),
                parent_id: None,
                role: "user".to_string(),
                content: "hello".to_string(),
                label: Some("greeting".to_string()),
            },
            TreeEntry::Compaction {
                node_id: "comp1".to_string(),
                parent_id: Some("m1".to_string()),
                summary: "summarized".to_string(),
                removed_count: 5,
            },
            TreeEntry::Branch {
                branch_id: "b1".to_string(),
                from_node_id: "m1".to_string(),
                label: Some("alt-approach".to_string()),
            },
            TreeEntry::ModelChange {
                node_id: "mc1".to_string(),
                previous: "sonnet".to_string(),
                current: "opus".to_string(),
            },
            TreeEntry::ThinkingLevel {
                node_id: "tl1".to_string(),
                level: "extended".to_string(),
            },
            TreeEntry::Custom {
                node_id: Some("m1".to_string()),
                key: "ext_data".to_string(),
                value: json!({"foo": 42}),
            },
            // Edge cases
            TreeEntry::Branch {
                branch_id: "b2".to_string(),
                from_node_id: "m1".to_string(),
                label: None,
            },
            TreeEntry::Custom {
                node_id: None,
                key: "global".to_string(),
                value: json!("any"),
            },
        ];

        for entry in &entries {
            let json = serde_json::to_string(entry).expect("serialize");
            let parsed: TreeEntry = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&parsed, entry, "round-trip failed for: {json}");
        }
    }

    // --- Truncated last line recovery ---

    #[test]
    fn load_skips_truncated_last_line() {
        let dir = std::env::temp_dir().join("claw_test_truncated");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("truncated.jsonl");

        // Write valid entry + truncated entry
        let valid_line = r#"{"type":"message","node_id":"r1","parent_id":null,"role":"user","content":"hi","label":null}"#;
        let truncated = r#"{"type":"message","node_id":"c1","parent"#;
        {
            let mut f = fs::File::create(&path).expect("create");
            writeln!(f, "{valid_line}").expect("write valid");
            write!(f, "{truncated}").expect("write truncated");
        }

        let log = SessionTreeLog::open(&path).expect("should recover from truncated last line");
        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.tree().root().unwrap().id, "r1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_fails_on_corrupted_middle_line() {
        let dir = std::env::temp_dir().join("claw_test_corrupted_mid");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("corrupted_mid.jsonl");

        let valid = r#"{"type":"message","node_id":"r1","parent_id":null,"role":"user","content":"hi","label":null}"#;
        let bad = r#"{not valid json}"#;
        let valid2 = r#"{"type":"message","node_id":"c1","parent_id":"r1","role":"assistant","content":"yo","label":null}"#;
        {
            let mut f = fs::File::create(&path).expect("create");
            writeln!(f, "{valid}").expect("write");
            writeln!(f, "{bad}").expect("write");
            writeln!(f, "{valid2}").expect("write");
        }

        let result = SessionTreeLog::open(&path);
        assert!(result.is_err(), "should fail on corrupted middle line");

        let _ = fs::remove_dir_all(&dir);
    }

    // --- Empty file ---

    #[test]
    fn open_empty_file_returns_empty_log() {
        let dir = std::env::temp_dir().join("claw_test_empty_file");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("empty.jsonl");

        fs::write(&path, "").expect("create empty file");

        let log = SessionTreeLog::open(&path).expect("open empty file");
        assert_eq!(log.entries().len(), 0);
        assert!(log.tree().root().is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    // --- ModelChange/ThinkingLevel errors when tree empty ---

    #[test]
    fn model_change_rejected_when_tree_empty() {
        let mut log = SessionTreeLog::new_in_memory();
        let result = log.append(TreeEntry::ModelChange {
            node_id: "mc1".to_string(),
            previous: "a".to_string(),
            current: "b".to_string(),
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no active node"));
    }

    #[test]
    fn thinking_level_rejected_when_tree_empty() {
        let mut log = SessionTreeLog::new_in_memory();
        let result = log.append(TreeEntry::ThinkingLevel {
            node_id: "tl1".to_string(),
            level: "extended".to_string(),
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no active node"));
    }

    // --- Branch without label ---

    #[test]
    fn branch_without_label_returns_none() {
        let mut log = SessionTreeLog::new_in_memory();
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "hi".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Branch {
            branch_id: "b1".to_string(),
            from_node_id: "r1".to_string(),
            label: None,
        })
        .expect("branch");

        assert!(log.branch_label("b1").is_none());
    }

    // --- Compaction + branch reconstruction ---

    #[test]
    fn compaction_then_branch_reconstructs_from_file() {
        let dir = std::env::temp_dir().join("claw_test_comp_branch");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("comp_branch.jsonl");
        let _ = fs::remove_file(&path);

        {
            let mut log = SessionTreeLog::open(&path).expect("open");
            log.append(TreeEntry::Message {
                node_id: "r1".to_string(),
                parent_id: None,
                role: "user".to_string(),
                content: "Long chat".to_string(),
                label: None,
            })
            .expect("r1");
            log.append(TreeEntry::Compaction {
                node_id: "comp1".to_string(),
                parent_id: Some("r1".to_string()),
                summary: "Summarized".to_string(),
                removed_count: 5,
            })
            .expect("comp1");
            log.append(TreeEntry::Branch {
                branch_id: "b1".to_string(),
                from_node_id: "comp1".to_string(),
                label: Some("alt".to_string()),
            })
            .expect("branch");
        }

        let log = SessionTreeLog::open(&path).expect("reopen");
        assert_eq!(log.entries().len(), 3);
        assert_eq!(log.tree().active().unwrap().id, "b1");
        assert_eq!(log.branch_label("b1"), Some("alt"));

        let _ = fs::remove_dir_all(&dir);
    }

    // --- build_session_context with branching ---

    #[test]
    fn build_context_excludes_inactive_branch() {
        let mut log = SessionTreeLog::new_in_memory();

        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Start".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Response".to_string(),
            label: None,
        })
        .expect("c1");
        log.append(TreeEntry::Branch {
            branch_id: "b1".to_string(),
            from_node_id: "c1".to_string(),
            label: None,
        })
        .expect("branch");
        // Add message on original branch (c1 → c2)
        log.append(TreeEntry::Message {
            node_id: "c2".to_string(),
            parent_id: Some("c1".to_string()),
            role: "user".to_string(),
            content: "Continue original".to_string(),
            label: None,
        })
        .expect("c2");

        // Active path is r1 → c1 → c2 (b1 is a fork, not on main path)
        let ctx = log.build_session_context();
        let ctx_ids: Vec<&str> = ctx.iter().map(|e| match e {
            TreeEntry::Message { node_id, .. } => node_id.as_str(),
            _ => "other",
        }).collect();
        assert_eq!(ctx_ids, vec!["r1", "c1", "c2"]);
    }

    // --- fork_to_new_file edge cases ---

    #[test]
    fn fork_at_root_produces_single_entry() {
        let dir = std::env::temp_dir().join("claw_test_fork_root");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("original.jsonl");
        let new_path = dir.join("forked_root.jsonl");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&new_path);

        let mut log = SessionTreeLog::open(&path).expect("open");
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Root".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Child".to_string(),
            label: None,
        })
        .expect("c1");

        let forked = log.fork_to_new_file("r1", &new_path).expect("fork at root");
        assert_eq!(forked.entries().len(), 1);
        assert_eq!(forked.tree().active().unwrap().id, "r1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_at_leaf_includes_all_ancestors() {
        let dir = std::env::temp_dir().join("claw_test_fork_leaf");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("original.jsonl");
        let new_path = dir.join("forked_leaf.jsonl");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&new_path);

        let mut log = SessionTreeLog::open(&path).expect("open");
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "A".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "B".to_string(),
            label: None,
        })
        .expect("c1");
        log.append(TreeEntry::Message {
            node_id: "c2".to_string(),
            parent_id: Some("c1".to_string()),
            role: "user".to_string(),
            content: "C".to_string(),
            label: None,
        })
        .expect("c2");
        log.append(TreeEntry::Message {
            node_id: "c3".to_string(),
            parent_id: Some("c2".to_string()),
            role: "assistant".to_string(),
            content: "D".to_string(),
            label: None,
        })
        .expect("c3");

        let forked = log.fork_to_new_file("c3", &new_path).expect("fork at leaf");
        assert_eq!(forked.entries().len(), 4);
        assert_eq!(forked.tree().active().unwrap().id, "c3");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_includes_branch_entries_on_ancestor_path() {
        let dir = std::env::temp_dir().join("claw_test_fork_branch");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("original.jsonl");
        let new_path = dir.join("forked_branch.jsonl");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&new_path);

        let mut log = SessionTreeLog::open(&path).expect("open");
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Start".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Response".to_string(),
            label: None,
        })
        .expect("c1");
        log.append(TreeEntry::Branch {
            branch_id: "b1".to_string(),
            from_node_id: "c1".to_string(),
            label: Some("alt".to_string()),
        })
        .expect("branch");
        log.append(TreeEntry::Message {
            node_id: "c2".to_string(),
            parent_id: Some("c1".to_string()),
            role: "user".to_string(),
            content: "Continue".to_string(),
            label: None,
        })
        .expect("c2");

        let forked = log.fork_to_new_file("c2", &new_path).expect("fork");
        // Should include r1, c1, branch(b1 from c1), c2
        assert!(forked.entries().len() >= 3);
        assert!(forked.tree().get("c2").is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    // --- Multi-branch reconstruction ---

    #[test]
    fn multi_branch_reconstruction_from_file() {
        let dir = std::env::temp_dir().join("claw_test_multi_branch");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("multi.jsonl");
        let _ = fs::remove_file(&path);

        {
            let mut log = SessionTreeLog::open(&path).expect("open");
            log.append(TreeEntry::Message {
                node_id: "r1".to_string(),
                parent_id: None,
                role: "user".to_string(),
                content: "Start".to_string(),
                label: None,
            })
            .expect("r1");
            log.append(TreeEntry::Message {
                node_id: "c1".to_string(),
                parent_id: Some("r1".to_string()),
                role: "assistant".to_string(),
                content: "Response".to_string(),
                label: None,
            })
            .expect("c1");
            log.append(TreeEntry::Branch {
                branch_id: "b1".to_string(),
                from_node_id: "c1".to_string(),
                label: Some("approach-a".to_string()),
            })
            .expect("branch a");
            // Navigate back and create second branch
            log.tree_mut().navigate_to("c1").expect("nav to c1");
            log.append(TreeEntry::Branch {
                branch_id: "b2".to_string(),
                from_node_id: "c1".to_string(),
                label: Some("approach-b".to_string()),
            })
            .expect("branch b");
        }

        let log = SessionTreeLog::open(&path).expect("reopen");
        assert_eq!(log.branch_label("b1"), Some("approach-a"));
        assert_eq!(log.branch_label("b2"), Some("approach-b"));
        assert!(log.tree().get("b1").is_some());
        assert!(log.tree().get("b2").is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    // --- Custom entry with matching node_id included in fork ---

    #[test]
    fn fork_includes_custom_entries_on_ancestor_nodes() {
        let dir = std::env::temp_dir().join("claw_test_fork_custom");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("original.jsonl");
        let new_path = dir.join("forked_custom.jsonl");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&new_path);

        let mut log = SessionTreeLog::open(&path).expect("open");
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Start".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Custom {
            node_id: Some("r1".to_string()),
            key: "ext_data".to_string(),
            value: json!({"foo": "bar"}),
        })
        .expect("custom on r1");
        log.append(TreeEntry::Custom {
            node_id: None,
            key: "global".to_string(),
            value: json!("orphan"),
        })
        .expect("global custom");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Response".to_string(),
            label: None,
        })
        .expect("c1");

        let forked = log.fork_to_new_file("c1", &new_path).expect("fork");
        // Should include r1 message, custom on r1, c1 message (but NOT global custom with None node_id)
        let custom_count = forked
            .entries()
            .iter()
            .filter(|e| matches!(e, TreeEntry::Custom { .. }))
            .count();
        assert_eq!(custom_count, 1, "should include only the custom with matching node_id");

        let _ = fs::remove_dir_all(&dir);
    }

    // --- Duplicate node_id behavior ---

    #[test]
    fn duplicate_node_id_as_child_rejected() {
        let mut log = SessionTreeLog::new_in_memory();
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Root".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "assistant".to_string(),
            content: "Child".to_string(),
            label: None,
        })
        .expect("c1");

        // Try to add another child with same id "c1"
        let result = log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("r1".to_string()),
            role: "user".to_string(),
            content: "Duplicate".to_string(),
            label: None,
        });
        assert!(result.is_err(), "duplicate node_id as child should be rejected");
    }

    // --- build_session_context with compaction on path ---

    #[test]
    fn build_context_includes_compaction_on_active_path() {
        let mut log = SessionTreeLog::new_in_memory();
        log.append(TreeEntry::Message {
            node_id: "r1".to_string(),
            parent_id: None,
            role: "user".to_string(),
            content: "Long chat".to_string(),
            label: None,
        })
        .expect("r1");
        log.append(TreeEntry::Compaction {
            node_id: "comp1".to_string(),
            parent_id: Some("r1".to_string()),
            summary: "Summarized".to_string(),
            removed_count: 5,
        })
        .expect("comp1");
        log.append(TreeEntry::Message {
            node_id: "c1".to_string(),
            parent_id: Some("comp1".to_string()),
            role: "user".to_string(),
            content: "After compaction".to_string(),
            label: None,
        })
        .expect("c1");

        let ctx = log.build_session_context();
        assert_eq!(ctx.len(), 3); // r1 message + compaction + c1 message
    }
}
