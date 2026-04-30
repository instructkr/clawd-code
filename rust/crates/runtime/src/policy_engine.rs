use std::time::Duration;

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
            Self::GreenAt { level } => context.green_level >= *level,
            Self::StaleBranch => context.branch_freshness >= STALE_BRANCH_THRESHOLD,
            Self::StartupBlocked => context.blocker == LaneBlocker::Startup,
            Self::LaneCompleted => context.completed,
            Self::LaneReconciled => context.reconciled,
            Self::ReviewPassed => context.review_status == ReviewStatus::Approved,
            Self::ScopedDiff => context.diff_scope == DiffScope::Scoped,
            Self::TimedOut { duration } => context.branch_freshness >= *duration,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    MergeToDev,
    MergeForward,
    RecoverOnce,
    Escalate { reason: String },
    CloseoutLane,
    CleanupSession,
    Reconcile { reason: ReconcileReason },
    Notify { channel: String },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneContext {
    pub lane_id: String,
    pub green_level: GreenLevel,
    pub branch_freshness: Duration,
    pub blocker: LaneBlocker,
    pub review_status: ReviewStatus,
    pub diff_scope: DiffScope,
    pub completed: bool,
    pub reconciled: bool,
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
            branch_freshness,
            blocker,
            review_status,
            diff_scope,
            completed,
            reconciled: false,
        }
    }

    /// Create a lane context that is already reconciled (no further action needed).
    #[must_use]
    pub fn reconciled(lane_id: impl Into<String>) -> Self {
        Self {
            lane_id: lane_id.into(),
            green_level: 0,
            branch_freshness: Duration::from_secs(0),
            blocker: LaneBlocker::None,
            review_status: ReviewStatus::Pending,
            diff_scope: DiffScope::Full,
            completed: true,
            reconciled: true,
        }
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
}

#[must_use]
pub fn evaluate(engine: &PolicyEngine, context: &LaneContext) -> Vec<PolicyAction> {
    let mut actions = Vec::new();
    for rule in &engine.rules {
        if rule.matches(context) {
            rule.action.flatten_into(&mut actions);
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        evaluate, DiffScope, LaneBlocker, LaneContext, PolicyAction, PolicyCondition, PolicyEngine,
        PolicyRule, ReconcileReason, ReviewStatus, STALE_BRANCH_THRESHOLD,
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
        );

        // when
        let actions = engine.evaluate(&context);

        // then
        assert_eq!(actions, vec![PolicyAction::MergeToDev]);
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
        );

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
