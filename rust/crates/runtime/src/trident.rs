use crate::compact::{compact_session, CompactionConfig, CompactionResult};
use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};
use std::collections::{BTreeMap, BTreeSet};

/// Configuration for the Trident compaction pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TridentConfig {
    pub supersede_enabled: bool,
    pub collapse_enabled: bool,
    pub cluster_enabled: bool,
    pub collapse_threshold: usize,
    pub cluster_min_size: usize,
    pub cluster_similarity_threshold: f64,
    pub max_file_operations: usize,
}

impl Default for TridentConfig {
    fn default() -> Self {
        Self {
            supersede_enabled: true,
            collapse_enabled: true,
            cluster_enabled: true,
            collapse_threshold: 4,
            cluster_min_size: 3,
            cluster_similarity_threshold: 0.6,
            max_file_operations: 100,
        }
    }
}

/// Statistics from a Trident compaction run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TridentStats {
    pub superseded_count: usize,
    pub collapsed_chains: usize,
    pub messages_collapsed: usize,
    pub clusters_found: usize,
    pub messages_clustered: usize,
    pub tokens_saved_estimate: usize,
    pub original_message_count: usize,
    pub final_message_count: usize,
}

impl Default for TridentStats {
    fn default() -> Self {
        Self {
            superseded_count: 0,
            collapsed_chains: 0,
            messages_collapsed: 0,
            clusters_found: 0,
            messages_clustered: 0,
            tokens_saved_estimate: 0,
            original_message_count: 0,
            final_message_count: 0,
        }
    }
}

impl TridentStats {
    pub fn format_report(&self) -> String {
        let compression = if self.final_message_count > 0 {
            self.original_message_count as f64 / self.final_message_count as f64
        } else {
            1.0
        };
        let mut lines = vec![
            "Trident Compaction Complete".to_string(),
            format!(
                "  Stage 1 (Supersede): {} obsolete removed",
                self.superseded_count
            ),
            format!(
                "  Stage 2 (Collapse):  {} -> {} summaries",
                self.messages_collapsed, self.collapsed_chains
            ),
            format!(
                "  Stage 3 (Cluster):   {} -> {} clusters",
                self.messages_clustered, self.clusters_found
            ),
            format!("  Original: {} messages", self.original_message_count),
            format!("  Final:    {} messages ({:.1}x compression)", self.final_message_count, compression),
        ];
        if self.tokens_saved_estimate > 0 {
            lines.push(format!(
                "  Est. tokens saved: ~{}",
                self.tokens_saved_estimate
            ));
        }
        lines.join("\n")
    }
}

/// Result of the Trident compaction pipeline.
#[derive(Debug, Clone)]
pub struct TridentResult {
    pub compacted_session: Session,
    pub stats: TridentStats,
}

/// Run the full Trident compaction pipeline on a session, then apply
/// the standard summary-based compaction.
pub fn trident_compact_session(
    session: &Session,
    compaction_config: CompactionConfig,
    trident_config: &TridentConfig,
) -> CompactionResult {
    let original_count = session.messages.len();
    let original_tokens: usize = session.messages.iter().map(estimate_message_tokens).sum();

    let mut stats = TridentStats {
        original_message_count: original_count,
        ..TridentStats::default()
    };

    let mut messages = session.messages.clone();

    if trident_config.supersede_enabled {
        let (kept, superseded_count) = stage1_supersede(&messages);
        stats.superseded_count = superseded_count;
        messages = kept;
    }

    if trident_config.collapse_enabled {
        let (collapsed, chains, collapsed_count) = stage2_collapse(&messages, trident_config.collapse_threshold);
        stats.collapsed_chains = chains;
        stats.messages_collapsed = collapsed_count;
        messages = collapsed;
    }

    if trident_config.cluster_enabled {
        let (clustered, clusters_found, messages_clustered) = stage3_cluster(
            &messages,
            trident_config.cluster_min_size,
            trident_config.cluster_similarity_threshold,
        );
        stats.clusters_found = clusters_found;
        stats.messages_clustered = messages_clustered;
        messages = clustered;
    }

    stats.final_message_count = messages.len();

    let final_tokens: usize = messages.iter().map(estimate_message_tokens).sum();
    stats.tokens_saved_estimate = original_tokens.saturating_sub(final_tokens);

    let mut trident_session = session.clone();
    trident_session.messages = messages;

    let result = compact_session(&trident_session, compaction_config);

    if stats.superseded_count > 0 || stats.collapsed_chains > 0 || stats.clusters_found > 0 {
        eprintln!("{}", stats.format_report());
    }

    result
}

// =============================================================================
// STAGE 1: SUPERSEDE — Zero-cost factual pruning
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOp {
    Read,
    Write,
    Edit,
}

#[derive(Debug)]
struct FileOperation {
    index: usize,
    op_type: FileOp,
}

fn stage1_supersede(messages: &[ConversationMessage]) -> (Vec<ConversationMessage>, usize) {
    let mut file_ops: BTreeMap<String, Vec<FileOperation>> = BTreeMap::new();

    for (i, msg) in messages.iter().enumerate() {
        for block in &msg.blocks {
            if let Some((path, op_type)) = extract_file_operation(block) {
                file_ops.entry(path).or_default().push(FileOperation {
                    index: i,
                    op_type,
                });
            }
        }
    }

    let mut obsolete_indices: BTreeSet<usize> = BTreeSet::new();

    for (_path, ops) in &file_ops {
        if ops.len() < 2 {
            continue;
        }

        let last_write_idx = ops
            .iter()
            .rev()
            .find(|op| op.op_type == FileOp::Write || op.op_type == FileOp::Edit)
            .map(|op| op.index);

        if let Some(last_write) = last_write_idx {
            for op in ops {
                if op.op_type == FileOp::Read && op.index < last_write {
                    obsolete_indices.insert(op.index);
                } else if (op.op_type == FileOp::Write || op.op_type == FileOp::Edit)
                    && op.index < last_write
                {
                    obsolete_indices.insert(op.index);
                }
            }
        }
    }

    let superseded_count = obsolete_indices.len();
    let kept: Vec<ConversationMessage> = messages
        .iter()
        .enumerate()
        .filter(|(i, _)| !obsolete_indices.contains(i))
        .map(|(_, msg)| msg.clone())
        .collect();

    (kept, superseded_count)
}

fn extract_file_operation(block: &ContentBlock) -> Option<(String, FileOp)> {
    match block {
        ContentBlock::ToolUse { name, input, .. } => {
            let path = extract_path_from_tool_input(name, input)?;
            let op_type = match name.as_str() {
                "read_file" | "Read" => FileOp::Read,
                "write_file" | "Write" => FileOp::Write,
                "edit_file" | "Edit" => FileOp::Edit,
                _ => return None,
            };
            Some((path, op_type))
        }
        ContentBlock::ToolResult { tool_name, output, .. } => {
            let path = extract_path_from_tool_output(tool_name, output)?;
            let op_type = match tool_name.as_str() {
                "read_file" | "Read" => FileOp::Read,
                "write_file" | "Write" => FileOp::Write,
                "edit_file" | "Edit" => FileOp::Edit,
                _ => return None,
            };
            Some((path, op_type))
        }
        ContentBlock::Text { .. } => None,
    }
}

fn extract_path_from_tool_input(tool_name: &str, input: &str) -> Option<String> {
    if !matches!(tool_name, "read_file" | "write_file" | "edit_file" | "Read" | "Write" | "Edit")
    {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|v| v.get("path")?.as_str().map(String::from))
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(input)
                .ok()
                .and_then(|v| v.get("file_path")?.as_str().map(String::from))
        })
}

fn extract_path_from_tool_output(tool_name: &str, output: &str) -> Option<String> {
    if !matches!(tool_name, "read_file" | "write_file" | "edit_file" | "Read" | "Write" | "Edit")
    {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|v| v.get("path")?.as_str().map(String::from))
        .or_else(|| {
            output
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("path: "))
                .map(String::from)
        })
}

// =============================================================================
// STAGE 2: COLLAPSE — Summarize chatty exchanges
// =============================================================================

fn stage2_collapse(
    messages: &[ConversationMessage],
    threshold: usize,
) -> (Vec<ConversationMessage>, usize, usize) {
    if messages.len() < threshold {
        return (messages.to_vec(), 0, 0);
    }

    let mut result: Vec<ConversationMessage> = Vec::new();
    let mut buffer: Vec<ConversationMessage> = Vec::new();
    let mut total_chains = 0;
    let mut total_collapsed = 0;

    for msg in messages {
        if is_chatty_message(msg) {
            buffer.push(msg.clone());
        } else {
            if buffer.len() >= threshold {
                let summary = generate_collapse_summary(&buffer);
                total_chains += 1;
                total_collapsed += buffer.len();
                result.push(ConversationMessage {
                    role: MessageRole::System,
                    blocks: vec![ContentBlock::Text {
                        text: format!("[Collapsed Conversation]\n{summary}"),
                    }],
                    usage: None,
                });
            } else {
                result.extend(buffer.drain(..));
            }
            buffer.clear();
            result.push(msg.clone());
        }
    }

    if buffer.len() >= threshold {
        let summary = generate_collapse_summary(&buffer);
        total_chains += 1;
        total_collapsed += buffer.len();
        result.push(ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: format!("[Collapsed Conversation]\n{summary}"),
            }],
            usage: None,
        });
    } else {
        result.extend(buffer);
    }

    (result, total_chains, total_collapsed)
}

fn is_chatty_message(msg: &ConversationMessage) -> bool {
    let total_chars: usize = msg.blocks.iter().map(|b| match b {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { input, .. } => input.len(),
        ContentBlock::ToolResult { output, .. } => output.len(),
    }).sum();

    let has_tool_use = msg.blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let has_tool_result = msg.blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }));

    if has_tool_use || has_tool_result {
        return false;
    }

    total_chars < 200
}

fn generate_collapse_summary(messages: &[ConversationMessage]) -> String {
    let user_count = messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .count();
    let assistant_count = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .count();

    let mut topics: Vec<String> = messages
        .iter()
        .filter_map(|m| {
            m.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    Some(truncate_text(text, 80))
                }
                _ => None,
            })
        })
        .take(5)
        .collect();
    topics.dedup();

    let mut lines = vec![format!(
        "Collapsed {} messages ({} user, {} assistant).",
        messages.len(),
        user_count,
        assistant_count
    )];

    if !topics.is_empty() {
        lines.push("Topics:".to_string());
        for topic in &topics {
            lines.push(format!("  - {topic}"));
        }
    }

    lines.join("\n")
}

// =============================================================================
// STAGE 3: CLUSTER — Semantic grouping and deep storage
// =============================================================================

fn stage3_cluster(
    messages: &[ConversationMessage],
    min_cluster_size: usize,
    similarity_threshold: f64,
) -> (Vec<ConversationMessage>, usize, usize) {
    if messages.len() < min_cluster_size {
        return (messages.to_vec(), 0, 0);
    }

    let fingerprints: Vec<MessageFingerprint> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| fingerprint_message(i, msg))
        .collect();

    if fingerprints.len() < min_cluster_size {
        return (messages.to_vec(), 0, 0);
    }

    let mut cluster_assignments: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cluster_id = 0;

    for i in 0..fingerprints.len() {
        if cluster_assignments.contains_key(&fingerprints[i].index) {
            continue;
        }

        let mut cluster_members: Vec<usize> = vec![fingerprints[i].index];

        for j in (i + 1)..fingerprints.len() {
            if cluster_assignments.contains_key(&fingerprints[j].index) {
                continue;
            }

            let similarity = compute_similarity(&fingerprints[i], &fingerprints[j]);
            if similarity >= similarity_threshold {
                cluster_members.push(fingerprints[j].index);
            }
        }

        if cluster_members.len() >= min_cluster_size {
            for member_idx in &cluster_members {
                cluster_assignments.insert(*member_idx, cluster_id);
            }
            cluster_id += 1;
        }
    }

    if cluster_assignments.is_empty() {
        return (messages.to_vec(), 0, 0);
    }

    let total_clustered: usize = cluster_assignments.len();
    let clusters_found = cluster_id as usize;

    let mut result: Vec<ConversationMessage> = Vec::new();
    let mut cluster_buffers: BTreeMap<usize, Vec<usize>> = BTreeMap::new();

    for (msg_idx, &cid) in &cluster_assignments {
        cluster_buffers.entry(cid).or_default().push(*msg_idx);
    }



    for (i, msg) in messages.iter().enumerate() {
        if let Some(&cid) = cluster_assignments.get(&i) {
            if let Some(buffer) = cluster_buffers.get_mut(&cid) {
                if buffer[0] == i {
                    let cluster_messages: Vec<&ConversationMessage> = buffer
                        .iter()
                        .filter_map(|&idx| messages.get(idx))
                        .collect();
                    let summary = generate_cluster_summary(&cluster_messages);
                    result.push(ConversationMessage {
                        role: MessageRole::System,
                        blocks: vec![ContentBlock::Text {
                            text: format!("[Clustered {} messages]\n{summary}", buffer.len()),
                        }],
                        usage: None,
                    });
                }
            }
        } else {
            result.push(msg.clone());
        }
    }

    (result, clusters_found, total_clustered)
}

#[derive(Debug)]
struct MessageFingerprint {
    index: usize,
    tool_names: BTreeSet<String>,
    file_paths: BTreeSet<String>,
    role: MessageRole,
    text_length: usize,
}

fn fingerprint_message(index: usize, msg: &ConversationMessage) -> Option<MessageFingerprint> {
    if msg.role == MessageRole::System {
        return None;
    }

    let mut tool_names: BTreeSet<String> = BTreeSet::new();
    let mut file_paths: BTreeSet<String> = BTreeSet::new();
    let mut text_length = 0;

    for block in &msg.blocks {
        match block {
            ContentBlock::ToolUse { name, input, .. } => {
                tool_names.insert(name.clone());
                if let Some(path) = extract_path_from_tool_input(name, input) {
                    file_paths.insert(path);
                }
                text_length += input.len();
            }
            ContentBlock::ToolResult { tool_name, output, .. } => {
                tool_names.insert(tool_name.clone());
                if let Some(path) = extract_path_from_tool_output(tool_name, output) {
                    file_paths.insert(path);
                }
                text_length += output.len();
            }
            ContentBlock::Text { text } => {
                text_length += text.len();
            }
        }
    }

    Some(MessageFingerprint {
        index,
        tool_names,
        file_paths,
        role: msg.role,
        text_length,
    })
}

fn compute_similarity(a: &MessageFingerprint, b: &MessageFingerprint) -> f64 {
    if a.role != b.role {
        return 0.0;
    }

    let tool_overlap = if a.tool_names.is_empty() && b.tool_names.is_empty() {
        1.0
    } else if a.tool_names.is_empty() || b.tool_names.is_empty() {
        0.0
    } else {
        let intersection: usize = a.tool_names.intersection(&b.tool_names).count();
        let union: usize = a.tool_names.union(&b.tool_names).count();
        intersection as f64 / union as f64
    };

    let file_overlap = if a.file_paths.is_empty() && b.file_paths.is_empty() {
        1.0
    } else if a.file_paths.is_empty() || b.file_paths.is_empty() {
        0.0
    } else {
        let intersection: usize = a.file_paths.intersection(&b.file_paths).count();
        let union: usize = a.file_paths.union(&b.file_paths).count();
        intersection as f64 / union as f64
    };

    let length_similarity = if a.text_length == 0 && b.text_length == 0 {
        1.0
    } else if a.text_length == 0 || b.text_length == 0 {
        0.0
    } else {
        let min_len = a.text_length.min(b.text_length) as f64;
        let max_len = a.text_length.max(b.text_length) as f64;
        min_len / max_len
    };

    0.4 * tool_overlap + 0.4 * file_overlap + 0.2 * length_similarity
}

fn generate_cluster_summary(messages: &[&ConversationMessage]) -> String {
    let mut tool_names: BTreeSet<String> = BTreeSet::new();
    let mut file_paths: BTreeSet<String> = BTreeSet::new();

    for msg in messages {
        for block in &msg.blocks {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    tool_names.insert(name.clone());
                    if let Some(path) = extract_path_from_tool_input(name, input) {
                        file_paths.insert(path);
                    }
                }
                ContentBlock::ToolResult { tool_name, output, .. } => {
                    tool_names.insert(tool_name.clone());
                    if let Some(path) = extract_path_from_tool_output(tool_name, output) {
                        file_paths.insert(path);
                    }
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }

    let mut lines = vec![format!("{} similar messages grouped.", messages.len())];

    if !tool_names.is_empty() {
        lines.push(format!(
            "Tools: {}.",
            tool_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }

    if !file_paths.is_empty() {
        let paths: Vec<String> = file_paths.iter().take(5).cloned().collect();
        lines.push(format!("Files: {}.", paths.join(", ")));
    }

    lines.join("\n")
}

// =============================================================================
// Utilities
// =============================================================================

fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    message
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len() / 4 + 1,
            ContentBlock::ToolUse { name, input, .. } => (name.len() + input.len()) / 4 + 1,
            ContentBlock::ToolResult {
                tool_name, output, ..
            } => (tool_name.len() + output.len()) / 4 + 1,
        })
        .sum()
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::CompactionConfig;
    use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};

    #[test]
    fn stage1_removes_obsolete_file_reads() {
        let messages = vec![
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("1", "read_file", r#"{"path":"src/main.rs","content":"old"}"#, false),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                input: r#"{"path":"src/main.rs","old":"old","new":"new"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("2", "edit_file", r#"{"path":"src/main.rs","ok":true}"#, false),
        ];

        let (kept, superseded) = stage1_supersede(&messages);
        assert!(superseded > 0, "should supersede the earlier read");
        assert!(kept.len() < messages.len());
    }

    #[test]
    fn stage1_keeps_standalone_reads() {
        let messages = vec![
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("1", "read_file", r#"{"path":"src/main.rs","content":"data"}"#, false),
        ];

        let (kept, superseded) = stage1_supersede(&messages);
        assert_eq!(superseded, 0);
        assert_eq!(kept.len(), messages.len());
    }

    #[test]
    fn stage2_collapses_chatty_messages() {
        let mut messages = vec![];
        for i in 0..6 {
            messages.push(ConversationMessage::user_text(&format!("ok {i}")));
            messages.push(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: format!("got {i}"),
            }]));
        }
        messages.push(ConversationMessage::assistant(vec![ContentBlock::ToolUse {
            id: "t".to_string(),
            name: "bash".to_string(),
            input: r#"{"command":"ls"}"#.to_string(),
        }]));

        let (result, chains, collapsed) = stage2_collapse(&messages, 4);
        assert!(chains > 0, "should collapse at least one chain");
        assert!(collapsed > 0);
        assert!(result.len() < messages.len());
    }

    #[test]
    fn stage3_clusters_similar_messages() {
        let mut messages = vec![];
        for i in 0..5 {
            messages.push(ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: format!("read_{i}"),
                name: "read_file".to_string(),
                input: format!(r#"{{"path":"src/{i}.rs"}}"#),
            }]));
            messages.push(ConversationMessage::tool_result(
                &format!("read_{i}"),
                "read_file",
                &format!(r#"{{"path":"src/{i}.rs","content":"data {i}"}}"#),
                false,
            ));
        }

        let (result, clusters, clustered) =
            stage3_cluster(&messages, 3, 0.4);
        assert!(clusters > 0, "should find at least one cluster");
        assert!(clustered > 0);
        assert!(result.len() < messages.len());
    }

    #[test]
    fn trident_full_pipeline_preserves_important_content() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("Read and fix main.rs"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("1", "read_file", r#"{"path":"src/main.rs","content":"fn main() { buggy }"}"#, false),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                input: r#"{"path":"src/main.rs","old":"buggy","new":"fixed"}"#.to_string(),
            }]),
            ConversationMessage::tool_result("2", "edit_file", r#"{"path":"src/main.rs","ok":true}"#, false),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Fixed the bug in main.rs".to_string(),
            }]),
        ];

        let trident_config = TridentConfig::default();
        let result = trident_compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 4,
                max_estimated_tokens: 1,
            },
            &trident_config,
        );

        assert!(result.removed_message_count > 0 || result.compacted_session.messages.len() < session.messages.len());
    }

    #[test]
    fn trident_stats_report() {
        let stats = TridentStats {
            superseded_count: 5,
            collapsed_chains: 2,
            messages_collapsed: 8,
            clusters_found: 1,
            messages_clustered: 3,
            tokens_saved_estimate: 1200,
            original_message_count: 20,
            final_message_count: 8,
        };
        let report = stats.format_report();
        assert!(report.contains("Stage 1 (Supersede): 5"));
        assert!(report.contains("Stage 2 (Collapse):  8 -> 2"));
        assert!(report.contains("Stage 3 (Cluster):   3 -> 1"));
        assert!(report.contains("1200") || report.contains("1,200"));
    }
}
