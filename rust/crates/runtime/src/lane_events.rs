#![allow(clippy::similar_names, clippy::cast_possible_truncation)]
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneEventName {
    #[serde(rename = "lane.started")]
    Started,
    #[serde(rename = "lane.ready")]
    Ready,
    #[serde(rename = "lane.prompt_misdelivery")]
    PromptMisdelivery,
    #[serde(rename = "lane.blocked")]
    Blocked,
    #[serde(rename = "lane.red")]
    Red,
    #[serde(rename = "lane.green")]
    Green,
    #[serde(rename = "lane.commit.created")]
    CommitCreated,
    #[serde(rename = "lane.pr.opened")]
    PrOpened,
    #[serde(rename = "lane.merge.ready")]
    MergeReady,
    #[serde(rename = "lane.finished")]
    Finished,
    #[serde(rename = "lane.failed")]
    Failed,
    #[serde(rename = "lane.reconciled")]
    Reconciled,
    #[serde(rename = "lane.merged")]
    Merged,
    #[serde(rename = "lane.superseded")]
    Superseded,
    #[serde(rename = "lane.closed")]
    Closed,
    #[serde(rename = "branch.stale_against_main")]
    BranchStaleAgainstMain,
    #[serde(rename = "branch.workspace_mismatch")]
    BranchWorkspaceMismatch,
    /// Ship/provenance events — §4.44.5
    #[serde(rename = "ship.prepared")]
    ShipPrepared,
    #[serde(rename = "ship.commits_selected")]
    ShipCommitsSelected,
    #[serde(rename = "ship.merged")]
    ShipMerged,
    #[serde(rename = "ship.pushed_main")]
    ShipPushedMain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneEventStatus {
    Running,
    Ready,
    Blocked,
    Red,
    Green,
    Completed,
    Failed,
    Reconciled,
    Merged,
    Superseded,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneFailureClass {
    PromptDelivery,
    TrustGate,
    BranchDivergence,
    Compile,
    Test,
    PluginStartup,
    McpStartup,
    McpHandshake,
    GatewayRouting,
    ToolRuntime,
    WorkspaceMismatch,
    Infra,
}

/// Provenance labels for event source classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProvenance {
    /// Event from a live, active lane
    LiveLane,
    /// Event from a synthetic test
    Test,
    /// Event from a healthcheck probe
    Healthcheck,
    /// Event from a replay/log replay
    Replay,
    /// Event from the transport layer itself
    Transport,
}

/// Session identity metadata captured at creation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdentity {
    /// Stable title for the session
    pub title: String,
    /// Workspace/worktree path
    pub workspace: String,
    /// Lane/session purpose
    pub purpose: String,
    /// Placeholder reason if any field is unknown
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder_reason: Option<String>,
}

impl SessionIdentity {
    /// Create complete session identity
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        workspace: impl Into<String>,
        purpose: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            workspace: workspace.into(),
            purpose: purpose.into(),
            placeholder_reason: None,
        }
    }

    /// Create session identity with placeholder for missing fields
    #[must_use]
    pub fn with_placeholder(
        title: impl Into<String>,
        workspace: impl Into<String>,
        purpose: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            workspace: workspace.into(),
            purpose: purpose.into(),
            placeholder_reason: Some(reason.into()),
        }
    }

    /// Reconcile enriched metadata onto this session identity.
    /// Updates fields with new information while preserving the session identity.
    /// Clears placeholder reason once real values are provided.
    #[must_use]
    pub fn reconcile_enriched(
        self,
        title: Option<String>,
        workspace: Option<String>,
        purpose: Option<String>,
    ) -> Self {
        // Check if any new values are provided before consuming options
        let has_new_data = title.is_some() || workspace.is_some() || purpose.is_some();
        Self {
            title: title.unwrap_or(self.title),
            workspace: workspace.unwrap_or(self.workspace),
            purpose: purpose.unwrap_or(self.purpose),
            // Clear placeholder if any real values were provided
            placeholder_reason: if has_new_data {
                None
            } else {
                self.placeholder_reason
            },
        }
    }
}

/// Lane ownership and workflow scope binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneOwnership {
    /// Owner/assignee identity
    pub owner: String,
    /// Workflow scope (e.g., claw-code-dogfood, external-git-maintenance)
    pub workflow_scope: String,
    /// Whether the watcher is expected to act, observe, or ignore
    pub watcher_action: WatcherAction,
}

/// Watcher action expectation for a lane event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherAction {
    /// Watcher should take action on this event
    Act,
    /// Watcher should only observe
    Observe,
    /// Watcher should ignore this event
    Ignore,
}

/// Confidence/trust level for downstream automation decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    /// High confidence - suitable for automated action
    High,
    /// Medium confidence - may require verification
    Medium,
    /// Low confidence - likely requires human review
    Low,
    /// Unknown confidence level
    Unknown,
}

/// Event metadata for ordering, provenance, deduplication, ownership, and confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEventMetadata {
    /// Monotonic sequence number for event ordering
    pub seq: u64,
    /// Event provenance source
    pub provenance: EventProvenance,
    /// Session identity at creation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<SessionIdentity>,
    /// Lane ownership and scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<LaneOwnership>,
    /// Nudge ID for deduplication cycles
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge_id: Option<String>,
    /// Event fingerprint for terminal event deduplication
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_fingerprint: Option<String>,
    /// Timestamp when event was observed/created
    pub timestamp_ms: u64,
    /// Environment/channel label (e.g., production, staging, dev)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_label: Option<String>,
    /// Emitter identity (e.g., clawd, plugin-name, operator-id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter_identity: Option<String>,
    /// Confidence/trust level for downstream automation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_level: Option<ConfidenceLevel>,
}

impl LaneEventMetadata {
    /// Create new event metadata
    #[must_use]
    pub fn new(seq: u64, provenance: EventProvenance) -> Self {
        Self {
            seq,
            provenance,
            session_identity: None,
            ownership: None,
            nudge_id: None,
            event_fingerprint: None,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            environment_label: None,
            emitter_identity: None,
            confidence_level: None,
        }
    }

    /// Add session identity
    #[must_use]
    pub fn with_session_identity(mut self, identity: SessionIdentity) -> Self {
        self.session_identity = Some(identity);
        self
    }

    /// Add ownership info
    #[must_use]
    pub fn with_ownership(mut self, ownership: LaneOwnership) -> Self {
        self.ownership = Some(ownership);
        self
    }

    /// Add nudge ID for dedupe
    #[must_use]
    pub fn with_nudge_id(mut self, nudge_id: impl Into<String>) -> Self {
        self.nudge_id = Some(nudge_id.into());
        self
    }

    /// Compute and add event fingerprint for terminal events
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.event_fingerprint = Some(fingerprint.into());
        self
    }

    /// Add environment/channel label
    #[must_use]
    pub fn with_environment(mut self, label: impl Into<String>) -> Self {
        self.environment_label = Some(label.into());
        self
    }

    /// Add emitter identity
    #[must_use]
    pub fn with_emitter(mut self, emitter: impl Into<String>) -> Self {
        self.emitter_identity = Some(emitter.into());
        self
    }

    /// Add confidence/trust level
    #[must_use]
    pub fn with_confidence(mut self, level: ConfidenceLevel) -> Self {
        self.confidence_level = Some(level);
        self
    }
}

/// Builder for constructing [`LaneEvent`]s with proper metadata.
#[derive(Debug, Clone)]
pub struct LaneEventBuilder {
    event: LaneEventName,
    status: LaneEventStatus,
    emitted_at: String,
    metadata: LaneEventMetadata,
    detail: Option<String>,
    failure_class: Option<LaneFailureClass>,
    data: Option<serde_json::Value>,
}

impl LaneEventBuilder {
    /// Start building a new lane event
    #[must_use]
    pub fn new(
        event: LaneEventName,
        status: LaneEventStatus,
        emitted_at: impl Into<String>,
        seq: u64,
        provenance: EventProvenance,
    ) -> Self {
        Self {
            event,
            status,
            emitted_at: emitted_at.into(),
            metadata: LaneEventMetadata::new(seq, provenance),
            detail: None,
            failure_class: None,
            data: None,
        }
    }

    /// Add session identity
    #[must_use]
    pub fn with_session_identity(mut self, identity: SessionIdentity) -> Self {
        self.metadata = self.metadata.with_session_identity(identity);
        self
    }

    /// Add ownership info
    #[must_use]
    pub fn with_ownership(mut self, ownership: LaneOwnership) -> Self {
        self.metadata = self.metadata.with_ownership(ownership);
        self
    }

    /// Add nudge ID
    #[must_use]
    pub fn with_nudge_id(mut self, nudge_id: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_nudge_id(nudge_id);
        self
    }

    /// Add detail
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Add failure class
    #[must_use]
    pub fn with_failure_class(mut self, failure_class: LaneFailureClass) -> Self {
        self.failure_class = Some(failure_class);
        self
    }

    /// Add data payload
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Add environment/channel label
    #[must_use]
    pub fn with_environment(mut self, label: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_environment(label);
        self
    }

    /// Add emitter identity
    #[must_use]
    pub fn with_emitter(mut self, emitter: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_emitter(emitter);
        self
    }

    /// Add confidence level
    #[must_use]
    pub fn with_confidence(mut self, level: ConfidenceLevel) -> Self {
        self.metadata = self.metadata.with_confidence(level);
        self
    }

    /// Compute fingerprint and build terminal event
    #[must_use]
    pub fn build_terminal(mut self) -> LaneEvent {
        let fingerprint = compute_event_fingerprint(&self.event, &self.status, self.data.as_ref());
        self.metadata = self.metadata.with_fingerprint(fingerprint);
        self.build()
    }

    /// Build the event
    #[must_use]
    pub fn build(self) -> LaneEvent {
        LaneEvent {
            event: self.event,
            status: self.status,
            emitted_at: self.emitted_at,
            failure_class: self.failure_class,
            detail: self.detail,
            data: self.data,
            metadata: self.metadata,
        }
    }
}

/// Check if an event kind is terminal (completed, failed, superseded, closed).
#[must_use]
pub fn is_terminal_event(event: LaneEventName) -> bool {
    matches!(
        event,
        LaneEventName::Finished
            | LaneEventName::Failed
            | LaneEventName::Superseded
            | LaneEventName::Closed
            | LaneEventName::Merged
    )
}

/// Compute a fingerprint for terminal event deduplication.
#[must_use]
pub fn compute_event_fingerprint(
    event: &LaneEventName,
    status: &LaneEventStatus,
    data: Option<&serde_json::Value>,
) -> String {
    use sha2::{Digest, Sha256};

    let payload = serde_json::json!({
        "event": event,
        "status": status,
        "data": data,
    });
    let canonical = serde_json::to_vec(&payload).unwrap_or_default();
    let digest = Sha256::digest(canonical);
    let mut fingerprint = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        write!(&mut fingerprint, "{byte:02x}").expect("writing to String should not fail");
    }
    fingerprint
}

/// Classification of event terminality for reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EventTerminality {
    /// Terminal event - represents final session outcome (completed, failed, etc.)
    Terminal,
    /// Advisory event - informational, not final outcome
    Advisory,
    /// Uncertainty event - transport died, terminal state unknown
    Uncertainty,
}

/// Determine the terminality classification of an event.
#[must_use]
#[allow(dead_code)]
pub fn classify_event_terminality(event: LaneEventName) -> EventTerminality {
    match event {
        LaneEventName::Finished
        | LaneEventName::Failed
        | LaneEventName::Merged
        | LaneEventName::Superseded
        | LaneEventName::Closed => EventTerminality::Terminal,
        LaneEventName::Reconciled => EventTerminality::Uncertainty,
        _ => EventTerminality::Advisory,
    }
}

/// Reconcile a burst of potentially contradictory events into one canonical outcome.
///
/// Handles:
/// - Out-of-order events (sorts by monotonic sequence)
/// - Duplicate terminal events (deduplicates by fingerprint)
/// - Transport death after terminal event (classifies as Uncertainty)
/// - `completed -> idle -> error -> completed` noise
#[must_use]
#[allow(dead_code)]
pub fn reconcile_terminal_events(events: &[LaneEvent]) -> Option<(LaneEvent, Vec<LaneEvent>)> {
    if events.is_empty() {
        return None;
    }

    // Sort by monotonic sequence number for deterministic ordering
    let mut sorted: Vec<LaneEvent> = events.to_vec();
    sorted.sort_by_key(|e| e.metadata.seq);

    // Track the last terminal event and any transport/uncertainty events after it
    let mut last_terminal: Option<LaneEvent> = None;
    let mut post_terminal_uncertainty = false;
    let mut reconciled_events = Vec::new();

    for event in &sorted {
        match classify_event_terminality(event.event) {
            EventTerminality::Terminal => {
                // Check if this is a duplicate of an already-seen terminal event
                if let Some(ref terminal) = last_terminal {
                    if let (Some(fp1), Some(fp2)) = (
                        &event.metadata.event_fingerprint,
                        &terminal.metadata.event_fingerprint,
                    ) {
                        if fp1 == fp2 {
                            // Same fingerprint - skip as duplicate
                            continue;
                        }
                    }
                    // Different terminal payload - check if materially different
                    if events_materially_differ(terminal, event) {
                        // Materially different terminal event - update to latest
                        last_terminal = Some(event.clone());
                    }
                } else {
                    last_terminal = Some(event.clone());
                }
            }
            EventTerminality::Uncertainty => {
                // Transport/server-down after terminal event creates uncertainty
                if last_terminal.is_some() {
                    post_terminal_uncertainty = true;
                }
                reconciled_events.push(event.clone());
            }
            EventTerminality::Advisory => {
                reconciled_events.push(event.clone());
            }
        }
    }

    // If there's post-terminal uncertainty, wrap the terminal event in uncertainty
    let final_terminal = if post_terminal_uncertainty {
        last_terminal.map(|mut t| {
            t.event = LaneEventName::Reconciled;
            t.status = LaneEventStatus::Reconciled;
            t.detail = Some(
                "Session terminal state uncertain: transport died after terminal event".to_string(),
            );
            t
        })
    } else {
        last_terminal
    };

    final_terminal.map(|t| (t, reconciled_events))
}

/// Check if two terminal events are materially different.
/// Used to determine if a later duplicate should override an earlier one.
#[must_use]
#[allow(dead_code)]
pub fn events_materially_differ(a: &LaneEvent, b: &LaneEvent) -> bool {
    // Different event type is material
    if a.event != b.event {
        return true;
    }

    // Different status is material
    if a.status != b.status {
        return true;
    }

    // Different failure class is material
    if a.failure_class != b.failure_class {
        return true;
    }

    // Different data payload is material
    if a.data != b.data {
        return true;
    }

    false
}

/// Filter events by provenance source.
#[must_use]
#[allow(dead_code)]
pub fn filter_by_provenance(events: &[LaneEvent], provenance: EventProvenance) -> Vec<LaneEvent> {
    events
        .iter()
        .filter(|e| e.metadata.provenance == provenance)
        .cloned()
        .collect()
}

/// Filter events by environment label.
#[must_use]
#[allow(dead_code)]
pub fn filter_by_environment(events: &[LaneEvent], environment: &str) -> Vec<LaneEvent> {
    events
        .iter()
        .filter(|e| {
            e.metadata
                .environment_label
                .as_ref()
                .is_some_and(|label| label == environment)
        })
        .cloned()
        .collect()
}

/// Filter events by minimum confidence level.
#[must_use]
#[allow(dead_code)]
pub fn filter_by_confidence(
    events: &[LaneEvent],
    min_confidence: ConfidenceLevel,
) -> Vec<LaneEvent> {
    let confidence_order = |c: ConfidenceLevel| match c {
        ConfidenceLevel::High => 3,
        ConfidenceLevel::Medium => 2,
        ConfidenceLevel::Low => 1,
        ConfidenceLevel::Unknown => 0,
    };
    let min_level = confidence_order(min_confidence);

    events
        .iter()
        .filter(|e| {
            e.metadata
                .confidence_level
                .is_some_and(|c| confidence_order(c) >= min_level)
        })
        .cloned()
        .collect()
}

/// Check if an event is from a test or synthetic source.
#[must_use]
#[allow(dead_code)]
pub fn is_test_event(event: &LaneEvent) -> bool {
    matches!(
        event.metadata.provenance,
        EventProvenance::Test | EventProvenance::Healthcheck | EventProvenance::Replay
    )
}

/// Check if an event is from a live production lane.
#[must_use]
#[allow(dead_code)]
pub fn is_live_lane_event(event: &LaneEvent) -> bool {
    event.metadata.provenance == EventProvenance::LiveLane
}

/// Nudge state tracking for acknowledgment and deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct NudgeTracking {
    /// Unique nudge/cycle identifier
    pub nudge_id: String,
    /// Timestamp when nudge was first delivered
    pub delivered_at: String,
    /// Whether this nudge has been acknowledged
    pub acknowledged: bool,
    /// Timestamp when acknowledged (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<String>,
    /// Whether this is a retry of a previous nudge
    pub is_retry: bool,
    /// Original nudge ID if this is a retry
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_nudge_id: Option<String>,
}

#[allow(dead_code)]
impl NudgeTracking {
    /// Create a new nudge tracking record
    #[must_use]
    pub fn new(nudge_id: impl Into<String>, delivered_at: impl Into<String>) -> Self {
        Self {
            nudge_id: nudge_id.into(),
            delivered_at: delivered_at.into(),
            acknowledged: false,
            acknowledged_at: None,
            is_retry: false,
            original_nudge_id: None,
        }
    }

    /// Create a nudge tracking record for a retry
    #[must_use]
    pub fn retry(
        nudge_id: impl Into<String>,
        delivered_at: impl Into<String>,
        original_nudge_id: impl Into<String>,
    ) -> Self {
        Self {
            nudge_id: nudge_id.into(),
            delivered_at: delivered_at.into(),
            acknowledged: false,
            acknowledged_at: None,
            is_retry: true,
            original_nudge_id: Some(original_nudge_id.into()),
        }
    }

    /// Mark this nudge as acknowledged
    #[must_use]
    pub fn acknowledge(mut self, at: impl Into<String>) -> Self {
        self.acknowledged = true;
        self.acknowledged_at = Some(at.into());
        self
    }
}

/// Classification of nudge types for deduplication logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NudgeClassification {
    /// Brand new nudge - first delivery
    New,
    /// Retry of a previous nudge (same content, new delivery)
    Retry,
    /// Stale duplicate - should be ignored
    StaleDuplicate,
}

/// Classify a nudge based on existing tracking records.
#[must_use]
#[allow(dead_code)]
pub fn classify_nudge(
    nudge_id: &str,
    existing_tracking: &[NudgeTracking],
    acknowledged_nudge_ids: &[String],
) -> NudgeClassification {
    // Check if already acknowledged - stale duplicate
    if acknowledged_nudge_ids.iter().any(|id| id == nudge_id) {
        return NudgeClassification::StaleDuplicate;
    }

    // Check if this is a retry of an existing nudge
    for tracking in existing_tracking {
        if tracking.nudge_id == nudge_id {
            // Same ID already seen - check if acknowledged
            if tracking.acknowledged {
                return NudgeClassification::StaleDuplicate;
            }
            // Not acknowledged yet - could be a retry with same ID
            return NudgeClassification::Retry;
        }

        // Check if this nudge is a retry of a tracked nudge
        if tracking.original_nudge_id.as_ref() == Some(&nudge_id.to_string()) {
            return NudgeClassification::StaleDuplicate;
        }
    }

    NudgeClassification::New
}

/// Stable roadmap ID assignment for newly filed pinpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RoadmapId {
    /// Canonical unique identifier
    pub id: String,
    /// Timestamp when first filed
    pub filed_at: String,
    /// Whether this is a new filing or update to existing
    pub is_new_filing: bool,
    /// Previous ID if this supersedes or merges another item
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

#[allow(dead_code)]
impl RoadmapId {
    /// Create a new roadmap ID at filing time
    #[must_use]
    pub fn new_filing(id: impl Into<String>, filed_at: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            filed_at: filed_at.into(),
            is_new_filing: true,
            supersedes: None,
        }
    }

    /// Create an update to an existing roadmap item
    #[must_use]
    pub fn update(id: impl Into<String>, filed_at: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            filed_at: filed_at.into(),
            is_new_filing: false,
            supersedes: None,
        }
    }

    /// Create a roadmap ID that supersedes another
    #[must_use]
    pub fn supersedes(
        id: impl Into<String>,
        filed_at: impl Into<String>,
        previous_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            filed_at: filed_at.into(),
            is_new_filing: true,
            supersedes: Some(previous_id.into()),
        }
    }
}

/// Lifecycle state for roadmap items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum RoadmapLifecycleState {
    /// Newly filed, awaiting acknowledgment
    Filed,
    /// Acknowledged by responsible party
    Acknowledged,
    /// Currently being worked on
    InProgress,
    /// Blocked on external dependency
    Blocked,
    /// Completed successfully
    Done,
    /// No longer relevant, replaced by another item
    Superseded,
}

/// Roadmap item lifecycle state with timestamp tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RoadmapLifecycle {
    /// Current lifecycle state
    pub state: RoadmapLifecycleState,
    /// Timestamp of last state change
    pub state_changed_at: String,
    /// Timestamp when first filed
    pub filed_at: String,
    /// Lineage for superseded/merged items
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub lineage: Vec<String>,
}

#[allow(dead_code)]
impl RoadmapLifecycle {
    /// Create a new roadmap lifecycle starting at "filed"
    #[must_use]
    pub fn new_filed(filed_at: impl Into<String>) -> Self {
        let filed_at = filed_at.into();
        Self {
            state: RoadmapLifecycleState::Filed,
            state_changed_at: filed_at.clone(),
            filed_at,
            lineage: Vec::new(),
        }
    }

    /// Transition to a new state
    #[must_use]
    pub fn transition(mut self, new_state: RoadmapLifecycleState, at: impl Into<String>) -> Self {
        self.state = new_state;
        self.state_changed_at = at.into();
        self
    }

    /// Mark as superseded by another item
    #[must_use]
    pub fn superseded_by(mut self, new_item_id: impl Into<String>, at: impl Into<String>) -> Self {
        let new_item_id = new_item_id.into();
        self.lineage.push(new_item_id.clone());
        self.state = RoadmapLifecycleState::Superseded;
        self.state_changed_at = at.into();
        self
    }

    /// Check if this item is in a terminal state (done or superseded)
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            RoadmapLifecycleState::Done | RoadmapLifecycleState::Superseded
        )
    }

    /// Check if this item is active (not terminal)
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }
}

/// Deduplicate terminal events within a reconciliation window.
/// Returns only the first occurrence of each terminal fingerprint.
#[must_use]
pub fn dedupe_terminal_events(events: &[LaneEvent]) -> Vec<LaneEvent> {
    let mut seen_fingerprints = std::collections::HashSet::new();
    let mut result = Vec::new();

    for event in events {
        if is_terminal_event(event.event) {
            if let Some(fp) = &event.metadata.event_fingerprint {
                if seen_fingerprints.contains(fp) {
                    continue; // Skip duplicate terminal event
                }
                seen_fingerprints.insert(fp.clone());
            }
        }
        result.push(event.clone());
    }

    result
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockedSubphase {
    #[serde(rename = "blocked.trust_prompt")]
    TrustPrompt { gate_repo: String },
    #[serde(rename = "blocked.prompt_delivery")]
    PromptDelivery { attempt: u32 },
    #[serde(rename = "blocked.plugin_init")]
    PluginInit { plugin_name: String },
    #[serde(rename = "blocked.mcp_handshake")]
    McpHandshake { server_name: String, attempt: u32 },
    #[serde(rename = "blocked.branch_freshness")]
    BranchFreshness { behind_main: u32 },
    #[serde(rename = "blocked.test_hang")]
    TestHang {
        elapsed_secs: u32,
        test_name: Option<String>,
    },
    #[serde(rename = "blocked.report_pending")]
    ReportPending { since_secs: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEventBlocker {
    #[serde(rename = "failureClass")]
    pub failure_class: LaneFailureClass,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subphase: Option<BlockedSubphase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCommitProvenance {
    pub commit: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(rename = "canonicalCommit", skip_serializing_if = "Option::is_none")]
    pub canonical_commit: Option<String>,
    #[serde(rename = "supersededBy", skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
}

/// Ship/provenance metadata — §4.44.5
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipProvenance {
    pub source_branch: String,
    pub base_commit: String,
    pub commit_count: u32,
    pub commit_range: String,
    pub merge_method: ShipMergeMethod,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipMergeMethod {
    DirectPush,
    FastForward,
    MergeCommit,
    SquashMerge,
    RebaseMerge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEvent {
    pub event: LaneEventName,
    pub status: LaneEventStatus,
    #[serde(rename = "emittedAt")]
    pub emitted_at: String,
    #[serde(rename = "failureClass", skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<LaneFailureClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Event metadata for ordering, provenance, dedupe, and ownership
    pub metadata: LaneEventMetadata,
}

impl LaneEvent {
    /// Create a new lane event with minimal metadata (seq=0, provenance=LiveLane)
    /// Use `LaneEventBuilder` for events requiring full metadata.
    #[must_use]
    pub fn new(
        event: LaneEventName,
        status: LaneEventStatus,
        emitted_at: impl Into<String>,
    ) -> Self {
        Self {
            event,
            status,
            emitted_at: emitted_at.into(),
            failure_class: None,
            detail: None,
            data: None,
            metadata: LaneEventMetadata::new(0, EventProvenance::LiveLane),
        }
    }

    #[must_use]
    pub fn started(emitted_at: impl Into<String>) -> Self {
        Self::new(LaneEventName::Started, LaneEventStatus::Running, emitted_at)
    }

    #[must_use]
    pub fn finished(emitted_at: impl Into<String>, detail: Option<String>) -> Self {
        Self::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            emitted_at,
        )
        .with_optional_detail(detail)
        .with_terminal_fingerprint()
    }

    #[must_use]
    pub fn commit_created(
        emitted_at: impl Into<String>,
        detail: Option<String>,
        provenance: LaneCommitProvenance,
    ) -> Self {
        Self::new(
            LaneEventName::CommitCreated,
            LaneEventStatus::Completed,
            emitted_at,
        )
        .with_optional_detail(detail)
        .with_data(serde_json::to_value(provenance).expect("commit provenance should serialize"))
    }

    #[must_use]
    pub fn superseded(
        emitted_at: impl Into<String>,
        detail: Option<String>,
        provenance: LaneCommitProvenance,
    ) -> Self {
        Self::new(
            LaneEventName::Superseded,
            LaneEventStatus::Superseded,
            emitted_at,
        )
        .with_optional_detail(detail)
        .with_data(serde_json::to_value(provenance).expect("commit provenance should serialize"))
    }

    #[must_use]
    pub fn blocked(emitted_at: impl Into<String>, blocker: &LaneEventBlocker) -> Self {
        let mut event = Self::new(LaneEventName::Blocked, LaneEventStatus::Blocked, emitted_at)
            .with_failure_class(blocker.failure_class)
            .with_detail(blocker.detail.clone());
        if let Some(ref subphase) = blocker.subphase {
            event =
                event.with_data(serde_json::to_value(subphase).expect("subphase should serialize"));
        }
        event
    }

    #[must_use]
    pub fn failed(emitted_at: impl Into<String>, blocker: &LaneEventBlocker) -> Self {
        let mut event = Self::new(LaneEventName::Failed, LaneEventStatus::Failed, emitted_at)
            .with_failure_class(blocker.failure_class)
            .with_detail(blocker.detail.clone());
        if let Some(ref subphase) = blocker.subphase {
            event =
                event.with_data(serde_json::to_value(subphase).expect("subphase should serialize"));
        }
        event.with_terminal_fingerprint()
    }

    /// Ship prepared — §4.44.5
    #[must_use]
    pub fn ship_prepared(emitted_at: impl Into<String>, provenance: &ShipProvenance) -> Self {
        Self::new(
            LaneEventName::ShipPrepared,
            LaneEventStatus::Ready,
            emitted_at,
        )
        .with_data(serde_json::to_value(provenance).expect("ship provenance should serialize"))
    }

    /// Ship commits selected — §4.44.5
    #[must_use]
    pub fn ship_commits_selected(
        emitted_at: impl Into<String>,
        commit_count: u32,
        commit_range: impl Into<String>,
    ) -> Self {
        Self::new(
            LaneEventName::ShipCommitsSelected,
            LaneEventStatus::Ready,
            emitted_at,
        )
        .with_detail(format!("{} commits: {}", commit_count, commit_range.into()))
    }

    /// Ship merged — §4.44.5
    #[must_use]
    pub fn ship_merged(emitted_at: impl Into<String>, provenance: &ShipProvenance) -> Self {
        Self::new(
            LaneEventName::ShipMerged,
            LaneEventStatus::Completed,
            emitted_at,
        )
        .with_data(serde_json::to_value(provenance).expect("ship provenance should serialize"))
    }

    /// Ship pushed to main — §4.44.5
    #[must_use]
    pub fn ship_pushed_main(emitted_at: impl Into<String>, provenance: &ShipProvenance) -> Self {
        Self::new(
            LaneEventName::ShipPushedMain,
            LaneEventStatus::Completed,
            emitted_at,
        )
        .with_data(serde_json::to_value(provenance).expect("ship provenance should serialize"))
    }

    #[must_use]
    pub fn with_failure_class(mut self, failure_class: LaneFailureClass) -> Self {
        self.failure_class = Some(failure_class);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_optional_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        if is_terminal_event(self.event) {
            self = self.with_terminal_fingerprint();
        }
        self
    }

    #[must_use]
    fn with_terminal_fingerprint(mut self) -> Self {
        if is_terminal_event(self.event) {
            self.metadata.event_fingerprint = Some(compute_event_fingerprint(
                &self.event,
                &self.status,
                self.data.as_ref(),
            ));
        }
        self
    }
}

#[must_use]
pub fn dedupe_superseded_commit_events(events: &[LaneEvent]) -> Vec<LaneEvent> {
    let mut keep = vec![true; events.len()];
    let mut latest_by_key = std::collections::BTreeMap::<String, usize>::new();

    for (index, event) in events.iter().enumerate() {
        if event.event != LaneEventName::CommitCreated {
            continue;
        }
        let Some(data) = event.data.as_ref() else {
            continue;
        };
        let key = data
            .get("canonicalCommit")
            .or_else(|| data.get("commit"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let superseded = data
            .get("supersededBy")
            .and_then(serde_json::Value::as_str)
            .is_some();
        if superseded {
            keep[index] = false;
            continue;
        }
        if let Some(key) = key {
            if let Some(previous) = latest_by_key.insert(key, index) {
                keep[previous] = false;
            }
        }
    }

    events
        .iter()
        .cloned()
        .zip(keep)
        .filter_map(|(event, retain)| retain.then_some(event))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        classify_event_terminality, compute_event_fingerprint, dedupe_superseded_commit_events,
        dedupe_terminal_events, events_materially_differ, filter_by_confidence,
        filter_by_environment, filter_by_provenance, is_live_lane_event, is_terminal_event,
        is_test_event, reconcile_terminal_events, BlockedSubphase, ConfidenceLevel,
        EventProvenance, EventTerminality, LaneCommitProvenance, LaneEvent, LaneEventBlocker,
        LaneEventBuilder, LaneEventMetadata, LaneEventName, LaneEventStatus, LaneFailureClass,
        LaneOwnership, SessionIdentity, ShipMergeMethod, ShipProvenance, WatcherAction,
    };

    #[test]
    fn canonical_lane_event_names_serialize_to_expected_wire_values() {
        let cases = [
            (LaneEventName::Started, "lane.started"),
            (LaneEventName::Ready, "lane.ready"),
            (LaneEventName::PromptMisdelivery, "lane.prompt_misdelivery"),
            (LaneEventName::Blocked, "lane.blocked"),
            (LaneEventName::Red, "lane.red"),
            (LaneEventName::Green, "lane.green"),
            (LaneEventName::CommitCreated, "lane.commit.created"),
            (LaneEventName::PrOpened, "lane.pr.opened"),
            (LaneEventName::MergeReady, "lane.merge.ready"),
            (LaneEventName::Finished, "lane.finished"),
            (LaneEventName::Failed, "lane.failed"),
            (LaneEventName::Reconciled, "lane.reconciled"),
            (LaneEventName::Merged, "lane.merged"),
            (LaneEventName::Superseded, "lane.superseded"),
            (LaneEventName::Closed, "lane.closed"),
            (
                LaneEventName::BranchStaleAgainstMain,
                "branch.stale_against_main",
            ),
            (
                LaneEventName::BranchWorkspaceMismatch,
                "branch.workspace_mismatch",
            ),
            (LaneEventName::ShipPrepared, "ship.prepared"),
            (LaneEventName::ShipCommitsSelected, "ship.commits_selected"),
            (LaneEventName::ShipMerged, "ship.merged"),
            (LaneEventName::ShipPushedMain, "ship.pushed_main"),
        ];

        for (event, expected) in cases {
            assert_eq!(
                serde_json::to_value(event).expect("serialize event"),
                json!(expected)
            );
        }
    }

    #[test]
    fn failure_classes_cover_canonical_taxonomy_wire_values() {
        let cases = [
            (LaneFailureClass::PromptDelivery, "prompt_delivery"),
            (LaneFailureClass::TrustGate, "trust_gate"),
            (LaneFailureClass::BranchDivergence, "branch_divergence"),
            (LaneFailureClass::Compile, "compile"),
            (LaneFailureClass::Test, "test"),
            (LaneFailureClass::PluginStartup, "plugin_startup"),
            (LaneFailureClass::McpStartup, "mcp_startup"),
            (LaneFailureClass::McpHandshake, "mcp_handshake"),
            (LaneFailureClass::GatewayRouting, "gateway_routing"),
            (LaneFailureClass::ToolRuntime, "tool_runtime"),
            (LaneFailureClass::WorkspaceMismatch, "workspace_mismatch"),
            (LaneFailureClass::Infra, "infra"),
        ];

        for (failure_class, expected) in cases {
            assert_eq!(
                serde_json::to_value(failure_class).expect("serialize failure class"),
                json!(expected)
            );
        }
    }

    #[test]
    fn blocked_and_failed_events_reuse_blocker_details() {
        let blocker = LaneEventBlocker {
            failure_class: LaneFailureClass::McpStartup,
            detail: "broken server".to_string(),
            subphase: Some(BlockedSubphase::McpHandshake {
                server_name: "test-server".to_string(),
                attempt: 1,
            }),
        };

        let blocked = LaneEvent::blocked("2026-04-04T00:00:00Z", &blocker);
        let failed = LaneEvent::failed("2026-04-04T00:00:01Z", &blocker);

        assert_eq!(blocked.event, LaneEventName::Blocked);
        assert_eq!(blocked.status, LaneEventStatus::Blocked);
        assert_eq!(blocked.failure_class, Some(LaneFailureClass::McpStartup));
        assert_eq!(failed.event, LaneEventName::Failed);
        assert_eq!(failed.status, LaneEventStatus::Failed);
        assert_eq!(failed.detail.as_deref(), Some("broken server"));
    }

    #[test]
    fn workspace_mismatch_failure_class_round_trips_in_branch_event_payloads() {
        let mismatch = LaneEvent::new(
            LaneEventName::BranchWorkspaceMismatch,
            LaneEventStatus::Blocked,
            "2026-04-04T00:00:02Z",
        )
        .with_failure_class(LaneFailureClass::WorkspaceMismatch)
        .with_detail("session belongs to /tmp/repo-a but current workspace is /tmp/repo-b")
        .with_data(json!({
            "expectedWorkspaceRoot": "/tmp/repo-a",
            "actualWorkspaceRoot": "/tmp/repo-b",
            "sessionId": "sess-123",
        }));

        let mismatch_json = serde_json::to_value(&mismatch).expect("lane event should serialize");
        assert_eq!(mismatch_json["event"], "branch.workspace_mismatch");
        assert_eq!(mismatch_json["failureClass"], "workspace_mismatch");
        assert_eq!(
            mismatch_json["data"]["expectedWorkspaceRoot"],
            "/tmp/repo-a"
        );

        let round_trip: LaneEvent =
            serde_json::from_value(mismatch_json).expect("lane event should deserialize");
        assert_eq!(round_trip.event, LaneEventName::BranchWorkspaceMismatch);
        assert_eq!(
            round_trip.failure_class,
            Some(LaneFailureClass::WorkspaceMismatch)
        );
    }

    #[test]
    fn ship_provenance_events_serialize_to_expected_wire_values() {
        let provenance = ShipProvenance {
            source_branch: "feature/provenance".to_string(),
            base_commit: "dd73962".to_string(),
            commit_count: 6,
            commit_range: "dd73962..c956f78".to_string(),
            merge_method: ShipMergeMethod::DirectPush,
            actor: "Jobdori".to_string(),
            pr_number: None,
        };

        let prepared = LaneEvent::ship_prepared("2026-04-20T14:30:00Z", &provenance);
        let prepared_json = serde_json::to_value(&prepared).expect("ship event should serialize");
        assert_eq!(prepared_json["event"], "ship.prepared");
        assert_eq!(prepared_json["data"]["commit_count"], 6);
        assert_eq!(prepared_json["data"]["source_branch"], "feature/provenance");

        let pushed = LaneEvent::ship_pushed_main("2026-04-20T14:35:00Z", &provenance);
        let pushed_json = serde_json::to_value(&pushed).expect("ship event should serialize");
        assert_eq!(pushed_json["event"], "ship.pushed_main");
        assert_eq!(pushed_json["data"]["merge_method"], "direct_push");

        let round_trip: LaneEvent =
            serde_json::from_value(pushed_json).expect("ship event should deserialize");
        assert_eq!(round_trip.event, LaneEventName::ShipPushedMain);
    }

    #[test]
    fn convenience_terminal_events_attach_and_refresh_fingerprints() {
        let finished = LaneEvent::finished("2026-04-04T00:00:00Z", Some("done".to_string()));
        let initial_fingerprint = finished
            .metadata
            .event_fingerprint
            .clone()
            .expect("finished events should carry terminal fingerprint");

        let with_payload = finished.with_data(json!({"result": "ok", "attempt": 1}));
        assert!(with_payload.metadata.event_fingerprint.is_some());
        assert_ne!(
            Some(initial_fingerprint),
            with_payload.metadata.event_fingerprint,
            "payload changes must refresh the actionable terminal fingerprint"
        );
    }

    #[test]
    fn tool_style_finished_events_dedupe_after_payload_is_added() {
        let first = LaneEvent::finished("2026-04-04T00:00:00Z", Some("done".to_string()))
            .with_data(json!({"result": "ok"}));
        let duplicate = LaneEvent::finished("2026-04-04T00:00:01Z", Some("done again".to_string()))
            .with_data(json!({"result": "ok"}));

        assert_eq!(
            first.metadata.event_fingerprint,
            duplicate.metadata.event_fingerprint
        );
        let deduped = dedupe_terminal_events(&[first, duplicate]);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn commit_events_can_carry_worktree_and_supersession_metadata() {
        let event = LaneEvent::commit_created(
            "2026-04-04T00:00:00Z",
            Some("commit created".to_string()),
            LaneCommitProvenance {
                commit: "abc123".to_string(),
                branch: "feature/provenance".to_string(),
                worktree: Some("wt-a".to_string()),
                canonical_commit: Some("abc123".to_string()),
                superseded_by: None,
                lineage: vec!["abc123".to_string()],
            },
        );
        let event_json = serde_json::to_value(&event).expect("lane event should serialize");
        assert_eq!(event_json["event"], "lane.commit.created");
        assert_eq!(event_json["data"]["branch"], "feature/provenance");
        assert_eq!(event_json["data"]["worktree"], "wt-a");
    }

    #[test]
    fn dedupes_superseded_commit_events_by_canonical_commit() {
        let retained = dedupe_superseded_commit_events(&[
            LaneEvent::commit_created(
                "2026-04-04T00:00:00Z",
                Some("old".to_string()),
                LaneCommitProvenance {
                    commit: "old123".to_string(),
                    branch: "feature/provenance".to_string(),
                    worktree: Some("wt-a".to_string()),
                    canonical_commit: Some("canon123".to_string()),
                    superseded_by: Some("new123".to_string()),
                    lineage: vec!["old123".to_string(), "new123".to_string()],
                },
            ),
            LaneEvent::commit_created(
                "2026-04-04T00:00:01Z",
                Some("new".to_string()),
                LaneCommitProvenance {
                    commit: "new123".to_string(),
                    branch: "feature/provenance".to_string(),
                    worktree: Some("wt-b".to_string()),
                    canonical_commit: Some("canon123".to_string()),
                    superseded_by: None,
                    lineage: vec!["old123".to_string(), "new123".to_string()],
                },
            ),
        ]);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].detail.as_deref(), Some("new"));
    }

    #[test]
    fn lane_event_metadata_includes_monotonic_sequence() {
        let meta1 = LaneEventMetadata::new(0, EventProvenance::LiveLane);
        let meta2 = LaneEventMetadata::new(1, EventProvenance::LiveLane);
        let meta3 = LaneEventMetadata::new(2, EventProvenance::Test);

        assert_eq!(meta1.seq, 0);
        assert_eq!(meta2.seq, 1);
        assert_eq!(meta3.seq, 2);
    }

    #[test]
    fn classify_event_terminality_correctly() {
        assert_eq!(
            classify_event_terminality(LaneEventName::Finished),
            EventTerminality::Terminal
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Failed),
            EventTerminality::Terminal
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Reconciled),
            EventTerminality::Uncertainty
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Started),
            EventTerminality::Advisory
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Ready),
            EventTerminality::Advisory
        );
    }

    #[test]
    fn event_provenance_round_trips_through_serialization() {
        let cases = [
            (EventProvenance::LiveLane, "live_lane"),
            (EventProvenance::Test, "test"),
            (EventProvenance::Healthcheck, "healthcheck"),
            (EventProvenance::Replay, "replay"),
            (EventProvenance::Transport, "transport"),
        ];

        for (provenance, expected) in cases {
            let json = serde_json::to_value(provenance).expect("should serialize");
            assert_eq!(json, serde_json::json!(expected));

            let round_trip: EventProvenance =
                serde_json::from_value(json).expect("should deserialize");
            assert_eq!(round_trip, provenance);
        }
    }

    #[test]
    fn session_identity_is_complete_at_creation() {
        let identity = SessionIdentity::new("my-lane", "/tmp/repo", "implement feature X");

        assert_eq!(identity.title, "my-lane");
        assert_eq!(identity.workspace, "/tmp/repo");
        assert_eq!(identity.purpose, "implement feature X");
        assert!(identity.placeholder_reason.is_none());

        // Test with placeholder
        let with_placeholder = SessionIdentity::with_placeholder(
            "untitled",
            "/tmp/unknown",
            "unknown",
            "session created before title was known",
        );
        assert_eq!(
            with_placeholder.placeholder_reason,
            Some("session created before title was known".to_string())
        );
    }

    #[test]
    fn session_identity_reconcile_enriched_updates_fields() {
        // Start with placeholder identity
        let initial = SessionIdentity::with_placeholder(
            "untitled",
            "/tmp/unknown",
            "unknown",
            "awaiting title from user",
        );
        assert!(initial.placeholder_reason.is_some());

        // Enrich with real title - workspace/purpose still unknown
        let enriched =
            initial.reconcile_enriched(Some("feature-branch-123".to_string()), None, None);
        assert_eq!(enriched.title, "feature-branch-123");
        assert_eq!(enriched.workspace, "/tmp/unknown"); // preserved
        assert_eq!(enriched.purpose, "unknown"); // preserved
                                                 // Placeholder cleared because we got a real title
        assert!(enriched.placeholder_reason.is_none());

        // Further enrichment with workspace and purpose
        let final_identity = enriched.reconcile_enriched(
            None, // keep existing title
            Some("/home/user/projects/my-app".to_string()),
            Some("implement user authentication".to_string()),
        );
        assert_eq!(final_identity.title, "feature-branch-123");
        assert_eq!(final_identity.workspace, "/home/user/projects/my-app");
        assert_eq!(final_identity.purpose, "implement user authentication");
        assert!(final_identity.placeholder_reason.is_none());
    }

    #[test]
    fn session_identity_reconcile_preserves_placeholder_if_no_new_data() {
        let initial = SessionIdentity::with_placeholder(
            "untitled",
            "/tmp/unknown",
            "unknown",
            "still waiting for info",
        );

        // Reconcile with no new data
        let reconciled = initial.reconcile_enriched(None, None, None);

        // Should preserve original values and placeholder
        assert_eq!(reconciled.title, "untitled");
        assert_eq!(reconciled.workspace, "/tmp/unknown");
        assert_eq!(reconciled.purpose, "unknown");
        assert_eq!(
            reconciled.placeholder_reason,
            Some("still waiting for info".to_string())
        );
    }

    #[test]
    fn lane_ownership_binding_includes_workflow_scope() {
        let ownership = LaneOwnership {
            owner: "claw-1".to_string(),
            workflow_scope: "claw-code-dogfood".to_string(),
            watcher_action: WatcherAction::Act,
        };

        assert_eq!(ownership.owner, "claw-1");
        assert_eq!(ownership.workflow_scope, "claw-code-dogfood");
        assert_eq!(ownership.watcher_action, WatcherAction::Act);
    }

    #[test]
    fn watcher_action_round_trips_through_serialization() {
        let cases = [
            (WatcherAction::Act, "act"),
            (WatcherAction::Observe, "observe"),
            (WatcherAction::Ignore, "ignore"),
        ];

        for (action, expected) in cases {
            let json = serde_json::to_value(action).expect("should serialize");
            assert_eq!(json, serde_json::json!(expected));

            let round_trip: WatcherAction =
                serde_json::from_value(json).expect("should deserialize");
            assert_eq!(round_trip, action);
        }
    }

    #[test]
    fn is_terminal_event_detects_terminal_states() {
        assert!(is_terminal_event(LaneEventName::Finished));
        assert!(is_terminal_event(LaneEventName::Failed));
        assert!(is_terminal_event(LaneEventName::Superseded));
        assert!(is_terminal_event(LaneEventName::Closed));
        assert!(is_terminal_event(LaneEventName::Merged));

        assert!(!is_terminal_event(LaneEventName::Started));
        assert!(!is_terminal_event(LaneEventName::Ready));
        assert!(!is_terminal_event(LaneEventName::Blocked));
    }

    #[test]
    fn compute_event_fingerprint_is_deterministic() {
        let fp1 = compute_event_fingerprint(
            &LaneEventName::Finished,
            &LaneEventStatus::Completed,
            Some(&json!({"commit": "abc123"})),
        );
        let fp2 = compute_event_fingerprint(
            &LaneEventName::Finished,
            &LaneEventStatus::Completed,
            Some(&json!({"commit": "abc123"})),
        );

        assert_eq!(fp1, fp2, "same inputs should produce same fingerprint");
        assert!(!fp1.is_empty());
        assert_eq!(fp1.len(), 16, "fingerprint should be 16 hex chars");
    }

    #[test]
    fn compute_event_fingerprint_differs_for_different_inputs() {
        let fp1 =
            compute_event_fingerprint(&LaneEventName::Finished, &LaneEventStatus::Completed, None);
        let fp2 = compute_event_fingerprint(&LaneEventName::Failed, &LaneEventStatus::Failed, None);
        let fp3 = compute_event_fingerprint(
            &LaneEventName::Finished,
            &LaneEventStatus::Completed,
            Some(&json!({"commit": "abc123"})),
        );

        assert_ne!(fp1, fp2, "different event/status should differ");
        assert_ne!(fp1, fp3, "different data should differ");
    }

    #[test]
    fn dedupe_terminal_events_suppresses_duplicates() {
        let event1 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .build_terminal();

        let event2 = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .build();

        let event3 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:02Z",
            2,
            EventProvenance::LiveLane,
        )
        .build_terminal(); // Same fingerprint as event1

        let deduped = dedupe_terminal_events(&[event1.clone(), event2.clone(), event3.clone()]);

        assert_eq!(deduped.len(), 2, "should have 2 events after dedupe");
        assert_eq!(deduped[0].event, LaneEventName::Finished);
        assert_eq!(deduped[1].event, LaneEventName::Started);
        // event3 should be suppressed as duplicate of event1
    }

    #[test]
    fn lane_event_builder_constructs_event_with_metadata() {
        let event = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:00Z",
            42,
            EventProvenance::Test,
        )
        .with_session_identity(SessionIdentity::new("test-lane", "/tmp", "test"))
        .with_ownership(LaneOwnership {
            owner: "bot-1".to_string(),
            workflow_scope: "test-suite".to_string(),
            watcher_action: WatcherAction::Observe,
        })
        .with_nudge_id("nudge-123")
        .with_detail("starting test run")
        .build();

        assert_eq!(event.event, LaneEventName::Started);
        assert_eq!(event.metadata.seq, 42);
        assert_eq!(event.metadata.provenance, EventProvenance::Test);
        assert_eq!(
            event.metadata.session_identity.as_ref().unwrap().title,
            "test-lane"
        );
        assert_eq!(event.metadata.ownership.as_ref().unwrap().owner, "bot-1");
        assert_eq!(event.metadata.nudge_id, Some("nudge-123".to_string()));
        assert_eq!(event.detail, Some("starting test run".to_string()));
    }

    #[test]
    fn lane_event_metadata_round_trips_through_serialization() {
        let meta = LaneEventMetadata::new(5, EventProvenance::Healthcheck)
            .with_session_identity(SessionIdentity::new("lane-1", "/tmp", "purpose"))
            .with_nudge_id("nudge-abc");

        let json = serde_json::to_value(&meta).expect("should serialize");
        assert_eq!(json["seq"], 5);
        assert_eq!(json["provenance"], "healthcheck");
        assert_eq!(json["nudge_id"], "nudge-abc");
        assert!(json["timestamp_ms"].as_u64().is_some());

        let round_trip: LaneEventMetadata =
            serde_json::from_value(json).expect("should deserialize");
        assert_eq!(round_trip.seq, 5);
        assert_eq!(round_trip.provenance, EventProvenance::Healthcheck);
        assert_eq!(round_trip.nudge_id, Some("nudge-abc".to_string()));
    }

    // US-013: Session event ordering + terminal-state reconciliation tests
    #[test]
    fn reconcile_terminal_events_sorts_by_monotonic_sequence() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Ready,
                LaneEventStatus::Ready,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .build(),
        ];

        let (terminal, _) = reconcile_terminal_events(&events).expect("should have terminal event");
        assert_eq!(terminal.event, LaneEventName::Finished);
        assert_eq!(terminal.metadata.seq, 2); // Highest sequence
    }

    #[test]
    fn reconcile_terminal_events_deduplicates_same_fingerprint() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
        ];

        let (terminal, _) = reconcile_terminal_events(&events).expect("should have terminal event");
        // Both have same fingerprint (same event/status/data), so should dedupe
        assert_eq!(terminal.event, LaneEventName::Finished);
    }

    #[test]
    fn reconcile_terminal_events_detects_transport_death_uncertainty() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
            LaneEventBuilder::new(
                LaneEventName::Reconciled,
                LaneEventStatus::Reconciled,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::Transport,
            )
            .build(),
        ];

        let (terminal, reconciled) =
            reconcile_terminal_events(&events).expect("should have result");
        // Transport death after terminal creates uncertainty
        assert_eq!(terminal.event, LaneEventName::Reconciled);
        assert_eq!(terminal.status, LaneEventStatus::Reconciled);
        assert!(terminal
            .detail
            .as_ref()
            .unwrap()
            .contains("transport died after terminal event"));
        assert_eq!(reconciled.len(), 1);
    }

    #[test]
    fn reconcile_terminal_events_handles_completed_idle_error_completed_noise() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Failed,
                LaneEventStatus::Failed,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .with_failure_class(LaneFailureClass::Infra)
            .build_terminal(),
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:03Z",
                3,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
        ];

        let (terminal, _) = reconcile_terminal_events(&events).expect("should have terminal event");
        // Latest terminal event wins
        assert_eq!(terminal.event, LaneEventName::Finished);
        assert_eq!(terminal.status, LaneEventStatus::Completed);
    }

    #[test]
    fn reconcile_terminal_events_returns_none_for_empty_input() {
        let result = reconcile_terminal_events(&[]);
        assert!(result.is_none());
    }

    #[test]
    fn reconcile_terminal_events_preserves_advisory_events() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Ready,
                LaneEventStatus::Ready,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Green,
                LaneEventStatus::Green,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .build(),
        ];

        let result = reconcile_terminal_events(&events);
        // Only advisory events - no terminal event to reconcile
        assert!(
            result.is_none(),
            "should return None when no terminal events"
        );
    }

    #[test]
    fn events_materially_differ_detects_real_differences() {
        let event_a = LaneEventBuilder::new(
            LaneEventName::Failed,
            LaneEventStatus::Failed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_failure_class(LaneFailureClass::Compile)
        .build_terminal();

        let event_b = LaneEventBuilder::new(
            LaneEventName::Failed,
            LaneEventStatus::Failed,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .with_failure_class(LaneFailureClass::Test)
        .build_terminal();

        assert!(events_materially_differ(&event_a, &event_b));
    }

    #[test]
    fn classify_event_terminality_correctly_classifies() {
        assert_eq!(
            classify_event_terminality(LaneEventName::Finished),
            EventTerminality::Terminal
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Failed),
            EventTerminality::Terminal
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Reconciled),
            EventTerminality::Uncertainty
        );
        assert_eq!(
            classify_event_terminality(LaneEventName::Started),
            EventTerminality::Advisory
        );
    }

    // US-014: Event provenance / environment labeling tests
    #[test]
    fn confidence_level_round_trips_through_serialization() {
        let cases = [
            (ConfidenceLevel::High, "high"),
            (ConfidenceLevel::Medium, "medium"),
            (ConfidenceLevel::Low, "low"),
            (ConfidenceLevel::Unknown, "unknown"),
        ];

        for (level, expected) in cases {
            let json = serde_json::to_value(level).expect("should serialize");
            assert_eq!(json, serde_json::json!(expected));

            let round_trip: ConfidenceLevel =
                serde_json::from_value(json).expect("should deserialize");
            assert_eq!(round_trip, level);
        }
    }

    #[test]
    fn filter_by_provenance_selects_only_matching_events() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Ready,
                LaneEventStatus::Ready,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::Test,
            )
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .build(),
        ];

        let live_events = filter_by_provenance(&events, EventProvenance::LiveLane);
        assert_eq!(live_events.len(), 2);
        assert_eq!(live_events[0].event, LaneEventName::Started);
        assert_eq!(live_events[1].event, LaneEventName::Finished);

        let test_events = filter_by_provenance(&events, EventProvenance::Test);
        assert_eq!(test_events.len(), 1);
        assert_eq!(test_events[0].event, LaneEventName::Ready);
    }

    #[test]
    fn filter_by_environment_selects_only_matching_environment() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .with_environment("production")
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Ready,
                LaneEventStatus::Ready,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .with_environment("staging")
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .with_environment("production")
            .build(),
        ];

        let prod_events = filter_by_environment(&events, "production");
        assert_eq!(prod_events.len(), 2);
        assert_eq!(prod_events[0].event, LaneEventName::Started);
        assert_eq!(prod_events[1].event, LaneEventName::Finished);

        let staging_events = filter_by_environment(&events, "staging");
        assert_eq!(staging_events.len(), 1);
        assert_eq!(staging_events[0].event, LaneEventName::Ready);
    }

    #[test]
    fn filter_by_confidence_selects_events_above_threshold() {
        let events = vec![
            LaneEventBuilder::new(
                LaneEventName::Started,
                LaneEventStatus::Running,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .with_confidence(ConfidenceLevel::High)
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Ready,
                LaneEventStatus::Ready,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .with_confidence(ConfidenceLevel::Medium)
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Blocked,
                LaneEventStatus::Blocked,
                "2026-04-04T00:00:02Z",
                2,
                EventProvenance::LiveLane,
            )
            .with_confidence(ConfidenceLevel::Low)
            .build(),
            LaneEventBuilder::new(
                LaneEventName::Failed,
                LaneEventStatus::Failed,
                "2026-04-04T00:00:03Z",
                3,
                EventProvenance::LiveLane,
            )
            // No confidence level set
            .build(),
        ];

        // High confidence filter should only return high confidence events
        let high_confidence = filter_by_confidence(&events, ConfidenceLevel::High);
        assert_eq!(high_confidence.len(), 1);
        assert_eq!(high_confidence[0].event, LaneEventName::Started);

        // Medium and above should return high and medium
        let medium_and_above = filter_by_confidence(&events, ConfidenceLevel::Medium);
        assert_eq!(medium_and_above.len(), 2);

        // Low and above should return high, medium, and low
        let low_and_above = filter_by_confidence(&events, ConfidenceLevel::Low);
        assert_eq!(low_and_above.len(), 3);
    }

    #[test]
    fn is_test_event_detects_synthetic_sources() {
        let test_event = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::Test,
        )
        .build();

        let healthcheck_event = LaneEventBuilder::new(
            LaneEventName::Ready,
            LaneEventStatus::Ready,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::Healthcheck,
        )
        .build();

        let live_event = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:02Z",
            2,
            EventProvenance::LiveLane,
        )
        .build();

        assert!(is_test_event(&test_event));
        assert!(is_test_event(&healthcheck_event));
        assert!(!is_test_event(&live_event));
    }

    #[test]
    fn is_live_lane_event_detects_production_events() {
        let live_event = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .build();

        let test_event = LaneEventBuilder::new(
            LaneEventName::Ready,
            LaneEventStatus::Ready,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::Test,
        )
        .build();

        assert!(is_live_lane_event(&live_event));
        assert!(!is_live_lane_event(&test_event));
    }

    #[test]
    fn lane_event_metadata_includes_us014_fields() {
        let meta = LaneEventMetadata::new(42, EventProvenance::LiveLane)
            .with_environment("production")
            .with_emitter("clawd-1")
            .with_confidence(ConfidenceLevel::High);

        assert_eq!(meta.environment_label, Some("production".to_string()));
        assert_eq!(meta.emitter_identity, Some("clawd-1".to_string()));
        assert_eq!(meta.confidence_level, Some(ConfidenceLevel::High));
    }

    // US-016: Duplicate terminal-event suppression tests
    #[test]
    fn canonical_terminal_event_fingerprint_attached_to_metadata() {
        let event = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_data(json!({"result": "success"}))
        .build_terminal();

        // Fingerprint should be computed and attached
        assert!(event.metadata.event_fingerprint.is_some());
        let fp = event.metadata.event_fingerprint.unwrap();
        assert_eq!(fp.len(), 16); // 16 hex characters
    }

    #[test]
    fn dedupe_terminal_events_suppresses_repeated_fingerprints() {
        let event1 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .build_terminal();

        let event2 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .build_terminal();

        // Both should have the same fingerprint (same event/status/data)
        assert_eq!(
            event1.metadata.event_fingerprint,
            event2.metadata.event_fingerprint
        );

        let deduped = dedupe_terminal_events(&[event1.clone(), event2.clone()]);
        // Should only keep first occurrence
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].metadata.seq, 0);
    }

    #[test]
    fn dedupe_preserves_raw_event_history_separately() {
        // This test demonstrates that raw events can be preserved
        // while exposing deduplicated actionable events
        let raw_events = vec![
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:00Z",
                0,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
            LaneEventBuilder::new(
                LaneEventName::Finished,
                LaneEventStatus::Completed,
                "2026-04-04T00:00:01Z",
                1,
                EventProvenance::LiveLane,
            )
            .build_terminal(),
        ];

        // Raw history preserved (2 events)
        assert_eq!(raw_events.len(), 2);

        // Deduplicated actionable events (1 event)
        let deduped = dedupe_terminal_events(&raw_events);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn events_materially_differ_detects_payload_differences() {
        let event_a = LaneEventBuilder::new(
            LaneEventName::Failed,
            LaneEventStatus::Failed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_failure_class(LaneFailureClass::Compile)
        .with_data(json!({"error": "compilation failed"}))
        .build_terminal();

        let event_b = LaneEventBuilder::new(
            LaneEventName::Failed,
            LaneEventStatus::Failed,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .with_failure_class(LaneFailureClass::Compile)
        .with_data(json!({"error": "different error message"}))
        .build_terminal();

        // Same event type, status, failure class - but different data payload
        assert!(events_materially_differ(&event_a, &event_b));
    }

    #[test]
    fn reconcile_terminal_events_surfaces_latest_when_different() {
        // Events with different data payloads will have different fingerprints
        let event1 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_data(json!({"attempt": 1, "result": "success"}))
        .build_terminal();

        let event2 = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .with_data(json!({"attempt": 2, "result": "success", "extra": "data"}))
        .build_terminal();

        // Fingerprints should differ due to different data
        assert_ne!(
            event1.metadata.event_fingerprint,
            event2.metadata.event_fingerprint
        );

        let (terminal, _) = reconcile_terminal_events(&[event1.clone(), event2.clone()])
            .expect("should have terminal");

        // Latest terminal event wins (seq 1, not seq 0) - data is different so it's material
        assert_eq!(terminal.metadata.seq, 1);
        assert_eq!(
            terminal.data,
            Some(json!({"attempt": 2, "result": "success", "extra": "data"}))
        );
    }

    // US-017: Lane ownership / scope binding tests
    #[test]
    fn lane_ownership_attached_to_metadata() {
        let ownership = LaneOwnership {
            owner: "bot-1".to_string(),
            workflow_scope: "claw-code-dogfood".to_string(),
            watcher_action: WatcherAction::Act,
        };

        let event = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_ownership(ownership.clone())
        .build();

        assert_eq!(event.metadata.ownership.as_ref().unwrap().owner, "bot-1");
        assert_eq!(
            event.metadata.ownership.as_ref().unwrap().workflow_scope,
            "claw-code-dogfood"
        );
        assert_eq!(
            event.metadata.ownership.as_ref().unwrap().watcher_action,
            WatcherAction::Act
        );
    }

    #[test]
    fn lane_ownership_preserved_through_lifecycle_events() {
        let ownership = LaneOwnership {
            owner: "operator-1".to_string(),
            workflow_scope: "external-git-maintenance".to_string(),
            watcher_action: WatcherAction::Observe,
        };

        let start_event = LaneEventBuilder::new(
            LaneEventName::Started,
            LaneEventStatus::Running,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_ownership(ownership.clone())
        .build();

        let ready_event = LaneEventBuilder::new(
            LaneEventName::Ready,
            LaneEventStatus::Ready,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .with_ownership(ownership.clone())
        .build();

        let finished_event = LaneEventBuilder::new(
            LaneEventName::Finished,
            LaneEventStatus::Completed,
            "2026-04-04T00:00:02Z",
            2,
            EventProvenance::LiveLane,
        )
        .with_ownership(ownership.clone())
        .build_terminal();

        // All events preserve ownership through the lifecycle
        assert_eq!(
            start_event.metadata.ownership.as_ref().unwrap().owner,
            "operator-1"
        );
        assert_eq!(
            ready_event.metadata.ownership.as_ref().unwrap().owner,
            "operator-1"
        );
        assert_eq!(
            finished_event.metadata.ownership.as_ref().unwrap().owner,
            "operator-1"
        );

        // Scope also preserved
        assert_eq!(
            start_event
                .metadata
                .ownership
                .as_ref()
                .unwrap()
                .workflow_scope,
            "external-git-maintenance"
        );
        assert_eq!(
            finished_event
                .metadata
                .ownership
                .as_ref()
                .unwrap()
                .workflow_scope,
            "external-git-maintenance"
        );
    }

    #[test]
    fn lane_ownership_watcher_action_variants() {
        let act_ownership = LaneOwnership {
            owner: "auto-bot".to_string(),
            workflow_scope: "infra-health".to_string(),
            watcher_action: WatcherAction::Act,
        };

        let observe_ownership = LaneOwnership {
            owner: "monitor-bot".to_string(),
            workflow_scope: "claw-code-dogfood".to_string(),
            watcher_action: WatcherAction::Observe,
        };

        let ignore_ownership = LaneOwnership {
            owner: "ignore-bot".to_string(),
            workflow_scope: "manual-operator".to_string(),
            watcher_action: WatcherAction::Ignore,
        };

        let act_event = LaneEventBuilder::new(
            LaneEventName::Blocked,
            LaneEventStatus::Blocked,
            "2026-04-04T00:00:00Z",
            0,
            EventProvenance::LiveLane,
        )
        .with_ownership(act_ownership)
        .build();

        let observe_event = LaneEventBuilder::new(
            LaneEventName::Ready,
            LaneEventStatus::Ready,
            "2026-04-04T00:00:01Z",
            1,
            EventProvenance::LiveLane,
        )
        .with_ownership(observe_ownership)
        .build();

        let ignore_event = LaneEventBuilder::new(
            LaneEventName::Green,
            LaneEventStatus::Green,
            "2026-04-04T00:00:02Z",
            2,
            EventProvenance::LiveLane,
        )
        .with_ownership(ignore_ownership)
        .build();

        assert_eq!(
            act_event
                .metadata
                .ownership
                .as_ref()
                .unwrap()
                .watcher_action,
            WatcherAction::Act
        );
        assert_eq!(
            observe_event
                .metadata
                .ownership
                .as_ref()
                .unwrap()
                .watcher_action,
            WatcherAction::Observe
        );
        assert_eq!(
            ignore_event
                .metadata
                .ownership
                .as_ref()
                .unwrap()
                .watcher_action,
            WatcherAction::Ignore
        );
    }
}
