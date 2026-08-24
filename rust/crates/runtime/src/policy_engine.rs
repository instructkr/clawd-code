use std::time::Duration;

use serde::{Deserialize, Serialize};

pub type GreenLevel = u8;

const STALE_BRANCH_THRESHOLD: Duration = Duration::from_hours(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRule {
    pub name: String,
    pub condition: PolicyCondition,
    pub action: PolicyAction,
    pub priority: u32,
}

impl PolicyRule {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        condition: PolicyCondition,
        action: PolicyAction,
        priority: u32,
    ) -> Self {
        Self {
            name: name.into(),
            condition,
            action,
            priority,
        }
    }

    #[must_use]
    pub fn matches(&self, context: &LaneContext) -> bool {
        self.condition.matches(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyCondition {
    And(Vec<PolicyCondition>),
    Or(Vec<PolicyCondition>),
    GreenAt { level: GreenLevel },
    StaleBranch,
    StartupBlocked,
    LaneCompleted,
    LaneReconciled,
    ReviewPassed,
    ScopedDiff,
    TimedOut { duration: Duration },
    RetryAvailable,
    RebaseRequired,
    StaleCleanupRequired,
    ApprovalTokenPresent,
    ApprovalTokenMissing,
}

impl PolicyCondition {
    #[must_use]
    pub fn matches(&self, context: &LaneContext) -> bool {
        match self {
            Self::And(conditions) => conditions
                .iter()
                .all(|condition| condition.matches(context)),
            Self::Or(conditions) => conditions
                .iter()
                .any(|condition| condition.matches(context)),
            Self::GreenAt { level } => {
                context.green_contract_satisfied && context.green_level >= *level
            }
            Self::StaleBranch => context.branch_freshness >= STALE_BRANCH_THRESHOLD,
            Self::StartupBlocked => context.blocker == LaneBlocker::Startup,
            Self::LaneCompleted => context.completed,
            Self::LaneReconciled => context.reconciled,
            Self::ReviewPassed => context.review_status == ReviewStatus::Approved,
            Self::ScopedDiff => context.diff_scope == DiffScope::Scoped,
            Self::TimedOut { duration } => context.branch_freshness >= *duration,
            Self::RetryAvailable => context.retry_count < context.retry_limit,
            Self::RebaseRequired => context.rebase_required,
            Self::StaleCleanupRequired => context.stale_cleanup_required,
            Self::ApprovalTokenPresent => context.approval_token.is_some(),
            Self::ApprovalTokenMissing => context.approval_token.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    MergeToDev,
    MergeForward,
    RecoverOnce,
    Retry { reason: String },
    Rebase { reason: String },
    Escalate { reason: String },
    CloseoutLane,
    CleanupSession,
    CleanupStale { reason: String },
    Reconcile { reason: ReconcileReason },
    Notify { channel: String },
    RequireApprovalToken { operation: String },
    Block { reason: String },
    Chain(Vec<PolicyAction>),
}

/// Why a lane was reconciled without further action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileReason {
    /// Branch already merged into main — no PR needed.
    AlreadyMerged,
    /// Work superseded by another lane or direct commit.
    Superseded,
    /// PR would be empty — all changes already landed.
    EmptyDiff,
    /// Lane manually closed by operator.
    ManualClose,
}

impl PolicyAction {
    fn flatten_into(&self, actions: &mut Vec<PolicyAction>) {
        match self {
            Self::Chain(chained) => {
                for action in chained {
                    action.flatten_into(actions);
                }
            }
            _ => actions.push(self.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneBlocker {
    None,
    Startup,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    Full,
    Scoped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalToken {
    pub token_id: String,
    pub operation: String,
    pub granted_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionKind {
    Retry,
    Rebase,
    Merge,
    Escalate,
    StaleCleanup,
    ApprovalRequired,
    Notify,
    Block,
    Closeout,
    Reconcile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecisionEvent {
    pub lane_id: String,
    pub rule_name: String,
    pub priority: u32,
    pub kind: PolicyDecisionKind,
    pub explanation: String,
    pub approval_token_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvaluation {
    pub actions: Vec<PolicyAction>,
    pub events: Vec<PolicyDecisionEvent>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneContext {
    pub lane_id: String,
    pub green_level: GreenLevel,
    pub green_contract_satisfied: bool,
    pub branch_freshness: Duration,
    pub blocker: LaneBlocker,
    pub review_status: ReviewStatus,
    pub diff_scope: DiffScope,
    pub completed: bool,
    pub reconciled: bool,
    pub retry_count: u32,
    pub retry_limit: u32,
    pub rebase_required: bool,
    pub stale_cleanup_required: bool,
    pub approval_token: Option<ApprovalToken>,
}

impl LaneContext {
    #[must_use]
    pub fn new(
        lane_id: impl Into<String>,
        green_level: GreenLevel,
        branch_freshness: Duration,
        blocker: LaneBlocker,
        review_status: ReviewStatus,
        diff_scope: DiffScope,
        completed: bool,
    ) -> Self {
        Self {
            lane_id: lane_id.into(),
            green_level,
            green_contract_satisfied: false,
            branch_freshness,
            blocker,
            review_status,
            diff_scope,
            completed,
            reconciled: false,
            retry_count: 0,
            retry_limit: 1,
            rebase_required: false,
            stale_cleanup_required: false,
            approval_token: None,
        }
    }

    /// Create a lane context that is already reconciled (no further action needed).
    #[must_use]
    pub fn reconciled(lane_id: impl Into<String>) -> Self {
        Self {
            lane_id: lane_id.into(),
            green_level: 0,
            green_contract_satisfied: false,
            branch_freshness: Duration::from_secs(0),
            blocker: LaneBlocker::None,
            review_status: ReviewStatus::Pending,
            diff_scope: DiffScope::Full,
            completed: true,
            reconciled: true,
            retry_count: 0,
            retry_limit: 1,
            rebase_required: false,
            stale_cleanup_required: false,
            approval_token: None,
        }
    }

    #[must_use]
    pub fn with_green_contract_satisfied(mut self, satisfied: bool) -> Self {
        self.green_contract_satisfied = satisfied;
        self
    }

    #[must_use]
    pub fn with_retry_state(mut self, retry_count: u32, retry_limit: u32) -> Self {
        self.retry_count = retry_count;
        self.retry_limit = retry_limit;
        self
    }

    #[must_use]
    pub fn with_rebase_required(mut self, required: bool) -> Self {
        self.rebase_required = required;
        self
    }

    #[must_use]
    pub fn with_stale_cleanup_required(mut self, required: bool) -> Self {
        self.stale_cleanup_required = required;
        self
    }

    #[must_use]
    pub fn with_approval_token(mut self, token: ApprovalToken) -> Self {
        self.approval_token = Some(token);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
}

impl PolicyEngine {
    #[must_use]
    pub fn new(mut rules: Vec<PolicyRule>) -> Self {
        rules.sort_by_key(|rule| rule.priority);
        Self { rules }
    }

    #[must_use]
    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    #[must_use]
    pub fn evaluate(&self, context: &LaneContext) -> Vec<PolicyAction> {
        evaluate(self, context)
    }

    #[must_use]
    pub fn evaluate_with_events(&self, context: &LaneContext) -> PolicyEvaluation {
        evaluate_with_events(self, context)
    }
}

#[must_use]
pub fn evaluate(engine: &PolicyEngine, context: &LaneContext) -> Vec<PolicyAction> {
    evaluate_with_events(engine, context).actions
}

#[must_use]
pub fn evaluate_with_events(engine: &PolicyEngine, context: &LaneContext) -> PolicyEvaluation {
    let mut actions = Vec::new();
    let mut events = Vec::new();
    for rule in &engine.rules {
        if rule.matches(context) {
            let before = actions.len();
            rule.action.flatten_into(&mut actions);
            for action in &actions[before..] {
                events.push(decision_event(rule, context, action));
            }
        }
    }
    PolicyEvaluation { actions, events }
}

fn decision_event(
    rule: &PolicyRule,
    context: &LaneContext,
    action: &PolicyAction,
) -> PolicyDecisionEvent {
    let (kind, explanation) = match action {
        PolicyAction::MergeToDev | PolicyAction::MergeForward => (
            PolicyDecisionKind::Merge,
            format!(
                "rule '{}' allows merge action for lane {}",
                rule.name, context.lane_id
            ),
        ),
        PolicyAction::RecoverOnce | PolicyAction::Retry { reason: _ } => (
            PolicyDecisionKind::Retry,
            format!(
                "rule '{}' allows retry {}/{} for lane {}",
                rule.name, context.retry_count, context.retry_limit, context.lane_id
            ),
        ),
        PolicyAction::Rebase { reason } => (
            PolicyDecisionKind::Rebase,
            format!("rule '{}' requires rebase: {reason}", rule.name),
        ),
        PolicyAction::Escalate { reason } => (
            PolicyDecisionKind::Escalate,
            format!(
                "rule '{}' escalates lane {}: {reason}",
                rule.name, context.lane_id
            ),
        ),
        PolicyAction::CleanupStale { reason } => (
            PolicyDecisionKind::StaleCleanup,
            format!("rule '{}' requests cleanup: {reason}", rule.name),
        ),
        PolicyAction::CleanupSession => (
            PolicyDecisionKind::StaleCleanup,
            format!("rule '{}' requests session cleanup", rule.name),
        ),
        PolicyAction::CloseoutLane => (
            PolicyDecisionKind::Closeout,
            format!("rule '{}' closes out lane {}", rule.name, context.lane_id),
        ),
        PolicyAction::Reconcile { reason } => (
            PolicyDecisionKind::Reconcile,
            format!(
                "rule '{}' reconciles lane {}: {reason:?}",
                rule.name, context.lane_id
            ),
        ),
        PolicyAction::Notify { channel } => (
            PolicyDecisionKind::Notify,
            format!("rule '{}' notifies {channel}", rule.name),
        ),
        PolicyAction::RequireApprovalToken { operation } => (
            PolicyDecisionKind::ApprovalRequired,
            format!(
                "rule '{}' requires approval token for {operation}",
                rule.name
            ),
        ),
        PolicyAction::Block { reason } => (
            PolicyDecisionKind::Block,
            format!(
                "rule '{}' blocks lane {}: {reason}",
                rule.name, context.lane_id
            ),
        ),
        PolicyAction::Chain(_) => (
            PolicyDecisionKind::Notify,
            format!("rule '{}' expanded a chained action", rule.name),
        ),
    };

    PolicyDecisionEvent {
        lane_id: context.lane_id.clone(),
        rule_name: rule.name.clone(),
        priority: rule.priority,
        kind,
        explanation,
        approval_token_id: context
            .approval_token
            .as_ref()
            .map(|token| token.token_id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        evaluate, ApprovalToken, DiffScope, LaneBlocker, LaneContext, PolicyAction,
        PolicyCondition, PolicyDecisionKind, PolicyEngine, PolicyRule, ReconcileReason,
        ReviewStatus, STALE_BRANCH_THRESHOLD,
    };

    fn default_context() -> LaneContext {
        LaneContext::new(
            "lane-7",
            0,
            Duration::from_secs(0),
            LaneBlocker::None,
            ReviewStatus::Pending,
            DiffScope::Full,
            false,
        )
    }

    #[test]
    fn merge_to_dev_rule_fires_for_green_scoped_reviewed_lane() {
        // given
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "merge-to-dev",
            PolicyCondition::And(vec![
                PolicyCondition::GreenAt { level: 2 },
                PolicyCondition::ScopedDiff,
                PolicyCondition::ReviewPassed,
            ]),
            PolicyAction::MergeToDev,
            20,
        )]);
        let context = LaneContext::new(
            "lane-7",
            3,
            Duration::from_secs(5),
            LaneBlocker::None,
            ReviewStatus::Approved,
            DiffScope::Scoped,
            false,
        )
        .with_green_contract_satisfied(true);

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(actions, vec![PolicyAction::MergeToDev]);
    }

    #[test]
    fn merge_rule_blocks_when_green_tests_lack_contract_provenance() {
        // given
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "merge-to-dev",
            PolicyCondition::And(vec![
                PolicyCondition::GreenAt { level: 2 },
                PolicyCondition::ScopedDiff,
                PolicyCondition::ReviewPassed,
            ]),
            PolicyAction::MergeToDev,
            20,
        )]);
        let context = LaneContext::new(
            "lane-7",
            3,
            Duration::from_secs(5),
            LaneBlocker::None,
            ReviewStatus::Approved,
            DiffScope::Scoped,
            false,
        );

        // when
        let actions = engine.evaluate(&context);

        // then
        assert!(actions.is_empty());
    }

    #[test]
    fn stale_branch_rule_fires_at_threshold() {
        // given
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "merge-forward",
            PolicyCondition::StaleBranch,
            PolicyAction::MergeForward,
            10,
        )]);
        let context = LaneContext::new(
            "lane-7",
            1,
            STALE_BRANCH_THRESHOLD,
            LaneBlocker::None,
            ReviewStatus::Pending,
            DiffScope::Full,
            false,
        );

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(actions, vec![PolicyAction::MergeForward]);
    }

    #[test]
    fn startup_blocked_rule_recovers_then_escalates() {
        // given
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "startup-recovery",
            PolicyCondition::StartupBlocked,
            PolicyAction::Chain(vec![
                PolicyAction::RecoverOnce,
                PolicyAction::Escalate {
                    reason: "startup remained blocked".to_string(),
                },
            ]),
            15,
        )]);
        let context = LaneContext::new(
            "lane-7",
            0,
            Duration::from_secs(0),
            LaneBlocker::Startup,
            ReviewStatus::Pending,
            DiffScope::Full,
            false,
        );

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(
            actions,
            vec![
                PolicyAction::RecoverOnce,
                PolicyAction::Escalate {
                    reason: "startup remained blocked".to_string(),
                },
            ]
        );
    }

    #[test]
    fn completed_lane_rule_closes_out_and_cleans_up() {
        // given
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "lane-closeout",
            PolicyCondition::LaneCompleted,
            PolicyAction::Chain(vec![
                PolicyAction::CloseoutLane,
                PolicyAction::CleanupSession,
            ]),
            30,
        )]);
        let context = LaneContext::new(
            "lane-7",
            0,
            Duration::from_secs(0),
            LaneBlocker::None,
            ReviewStatus::Pending,
            DiffScope::Full,
            true,
        );

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(
            actions,
            vec![PolicyAction::CloseoutLane, PolicyAction::CleanupSession]
        );
    }

    #[test]
    fn matching_rules_are_returned_in_priority_order_with_stable_ties() {
        // given
        let engine = PolicyEngine::new(vec![
            PolicyRule::new(
                "late-cleanup",
                PolicyCondition::And(vec![]),
                PolicyAction::CleanupSession,
                30,
            ),
            PolicyRule::new(
                "first-notify",
                PolicyCondition::And(vec![]),
                PolicyAction::Notify {
                    channel: "ops".to_string(),
                },
                10,
            ),
            PolicyRule::new(
                "second-notify",
                PolicyCondition::And(vec![]),
                PolicyAction::Notify {
                    channel: "review".to_string(),
                },
                10,
            ),
            PolicyRule::new(
                "merge",
                PolicyCondition::And(vec![]),
                PolicyAction::MergeToDev,
                20,
            ),
        ]);
        let context = default_context();

        // when
        let actions = evaluate(&engine, &context);

        // then
        assert_eq!(
            actions,
            vec![
                PolicyAction::Notify {
                    channel: "ops".to_string(),
                },
                PolicyAction::Notify {
                    channel: "review".to_string(),
                },
                PolicyAction::MergeToDev,
                PolicyAction::CleanupSession,
            ]
        );
    }

    #[test]
    fn combinators_handle_empty_cases_and_nested_chains() {
        // given
        let engine = PolicyEngine::new(vec![
            PolicyRule::new(
                "empty-and",
                PolicyCondition::And(vec![]),
                PolicyAction::Notify {
                    channel: "orchestrator".to_string(),
                },
                5,
            ),
            PolicyRule::new(
                "empty-or",
                PolicyCondition::Or(vec![]),
                PolicyAction::Block {
                    reason: "should not fire".to_string(),
                },
                10,
            ),
            PolicyRule::new(
                "nested",
                PolicyCondition::Or(vec![
                    PolicyCondition::StartupBlocked,
                    PolicyCondition::And(vec![
                        PolicyCondition::GreenAt { level: 2 },
                        PolicyCondition::TimedOut {
                            duration: Duration::from_secs(5),
                        },
                    ]),
                ]),
                PolicyAction::Chain(vec![
                    PolicyAction::Notify {
                        channel: "alerts".to_string(),
                    },
                    PolicyAction::Chain(vec![
                        PolicyAction::MergeForward,
                        PolicyAction::CleanupSession,
                    ]),
                ]),
                15,
            ),
        ]);
        let context = LaneContext::new(
            "lane-7",
            2,
            Duration::from_secs(10),
            LaneBlocker::External,
            ReviewStatus::Pending,
            DiffScope::Full,
            false,
        )
        .with_green_contract_satisfied(true);

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(
            actions,
            vec![
                PolicyAction::Notify {
                    channel: "orchestrator".to_string(),
                },
                PolicyAction::Notify {
                    channel: "alerts".to_string(),
                },
                PolicyAction::MergeForward,
                PolicyAction::CleanupSession,
            ]
        );
    }

    #[test]
    #[allow(clippy::duration_suboptimal_units, clippy::too_many_lines)]
    fn executable_decision_table_emits_retry_rebase_merge_escalate_cleanup_and_approval_events() {
        let engine = PolicyEngine::new(vec![
            PolicyRule::new(
                "retry-available",
                PolicyCondition::RetryAvailable,
                PolicyAction::Retry {
                    reason: "transient failure".to_string(),
                },
                1,
            ),
            PolicyRule::new(
                "rebase-required",
                PolicyCondition::RebaseRequired,
                PolicyAction::Rebase {
                    reason: "base branch moved".to_string(),
                },
                2,
            ),
            PolicyRule::new(
                "stale-cleanup",
                PolicyCondition::StaleCleanupRequired,
                PolicyAction::CleanupStale {
                    reason: "lease expired".to_string(),
                },
                3,
            ),
            PolicyRule::new(
                "approval-required",
                PolicyCondition::ApprovalTokenMissing,
                PolicyAction::RequireApprovalToken {
                    operation: "merge".to_string(),
                },
                4,
            ),
            PolicyRule::new(
                "merge-approved",
                PolicyCondition::And(vec![
                    PolicyCondition::ApprovalTokenPresent,
                    PolicyCondition::GreenAt { level: 2 },
                    PolicyCondition::ScopedDiff,
                    PolicyCondition::ReviewPassed,
                ]),
                PolicyAction::MergeToDev,
                5,
            ),
            PolicyRule::new(
                "retry-exhausted",
                PolicyCondition::TimedOut {
                    duration: Duration::from_secs(60),
                },
                PolicyAction::Escalate {
                    reason: "lane timed out".to_string(),
                },
                6,
            ),
        ]);

        let missing_token_context = LaneContext::new(
            "lane-cc2",
            2,
            Duration::from_secs(90),
            LaneBlocker::None,
            ReviewStatus::Approved,
            DiffScope::Scoped,
            false,
        )
        .with_green_contract_satisfied(true)
        .with_retry_state(0, 1)
        .with_rebase_required(true)
        .with_stale_cleanup_required(true);

        let missing = engine.evaluate_with_events(&missing_token_context);
        assert!(missing.actions.contains(&PolicyAction::Retry {
            reason: "transient failure".to_string()
        }));
        assert!(missing.actions.contains(&PolicyAction::Rebase {
            reason: "base branch moved".to_string()
        }));
        assert!(missing.actions.contains(&PolicyAction::CleanupStale {
            reason: "lease expired".to_string()
        }));
        assert!(missing
            .actions
            .contains(&PolicyAction::RequireApprovalToken {
                operation: "merge".to_string()
            }));
        assert!(missing.actions.contains(&PolicyAction::Escalate {
            reason: "lane timed out".to_string()
        }));
        assert!(missing
            .events
            .iter()
            .any(|event| event.kind == PolicyDecisionKind::ApprovalRequired
                && event.explanation.contains("approval token")));

        let approved_context = missing_token_context.with_approval_token(ApprovalToken {
            token_id: "approval-123".to_string(),
            operation: "merge".to_string(),
            granted_by: "leader".to_string(),
        });
        let approved = engine.evaluate_with_events(&approved_context);
        assert!(approved.actions.contains(&PolicyAction::MergeToDev));
        let merge_event = approved
            .events
            .iter()
            .find(|event| event.kind == PolicyDecisionKind::Merge)
            .expect("merge event should be emitted");
        assert_eq!(
            merge_event.approval_token_id.as_deref(),
            Some("approval-123")
        );
    }

    #[test]
    fn reconciled_lane_emits_reconcile_and_cleanup() {
        // given — a lane where branch is already merged, no PR needed, session stale
        let engine = PolicyEngine::new(vec![
            PolicyRule::new(
                "reconcile-closeout",
                PolicyCondition::LaneReconciled,
                PolicyAction::Chain(vec![
                    PolicyAction::Reconcile {
                        reason: ReconcileReason::AlreadyMerged,
                    },
                    PolicyAction::CloseoutLane,
                    PolicyAction::CleanupSession,
                ]),
                5,
            ),
            // This rule should NOT fire — reconciled lanes are completed but we want
            // the more specific reconcile rule to handle them
            PolicyRule::new(
                "generic-closeout",
                PolicyCondition::And(vec![
                    PolicyCondition::LaneCompleted,
                    // Only fire if NOT reconciled
                    PolicyCondition::And(vec![]),
                ]),
                PolicyAction::CloseoutLane,
                30,
            ),
        ]);
        let context = LaneContext::reconciled("lane-9411");

        // when
        let actions = engine.evaluate(&context);

        // then — reconcile rule fires first (priority 5), then generic closeout also fires
        // because reconciled context has completed=true
        assert_eq!(
            actions,
            vec![
                PolicyAction::Reconcile {
                    reason: ReconcileReason::AlreadyMerged,
                },
                PolicyAction::CloseoutLane,
                PolicyAction::CleanupSession,
                PolicyAction::CloseoutLane,
            ]
        );
    }

    #[test]
    fn reconciled_context_has_correct_defaults() {
        let ctx = LaneContext::reconciled("test-lane");
        assert_eq!(ctx.lane_id, "test-lane");
        assert!(ctx.completed);
        assert!(ctx.reconciled);
        assert_eq!(ctx.blocker, LaneBlocker::None);
        assert_eq!(ctx.green_level, 0);
    }

    #[test]
    fn non_reconciled_lane_does_not_trigger_reconcile_rule() {
        let engine = PolicyEngine::new(vec![PolicyRule::new(
            "reconcile-closeout",
            PolicyCondition::LaneReconciled,
            PolicyAction::Reconcile {
                reason: ReconcileReason::EmptyDiff,
            },
            5,
        )]);
        // Normal completed lane — not reconciled
        let context = LaneContext::new(
            "lane-7",
            0,
            Duration::from_secs(0),
            LaneBlocker::None,
            ReviewStatus::Pending,
            DiffScope::Full,
            true,
        );

        let actions = engine.evaluate(&context);
        assert!(actions.is_empty());
    }

    #[test]
    fn reconcile_reason_variants_are_distinct() {
        assert_ne!(ReconcileReason::AlreadyMerged, ReconcileReason::Superseded);
        assert_ne!(ReconcileReason::EmptyDiff, ReconcileReason::ManualClose);
    }
}
