#![allow(clippy::cast_possible_truncation, clippy::uninlined_format_args)]
//! Recovery recipes for common failure scenarios.
//!
//! Encodes known automatic recoveries for the six failure scenarios
//! listed in ROADMAP item 8, and enforces one automatic recovery
//! attempt before escalation. Each attempt is emitted as a structured
//! recovery event.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::worker_boot::WorkerFailureKind;

/// The six failure scenarios that have known recovery recipes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureScenario {
    TrustPromptUnresolved,
    PromptMisdelivery,
    StaleBranch,
    CompileRedCrossCrate,
    McpHandshakeFailure,
    PartialPluginStartup,
    ProviderFailure,
}

impl FailureScenario {
    /// Returns all known failure scenarios.
    #[must_use]
    pub fn all() -> &'static [FailureScenario] {
        &[
            Self::TrustPromptUnresolved,
            Self::PromptMisdelivery,
            Self::StaleBranch,
            Self::CompileRedCrossCrate,
            Self::McpHandshakeFailure,
            Self::PartialPluginStartup,
            Self::ProviderFailure,
        ]
    }

    /// Map a `WorkerFailureKind` to the corresponding `FailureScenario`.
    /// This is the bridge that lets recovery policy consume worker boot events.
    #[must_use]
    pub fn from_worker_failure_kind(kind: WorkerFailureKind) -> Self {
        match kind {
            WorkerFailureKind::TrustGate | WorkerFailureKind::ToolPermissionGate => {
                Self::TrustPromptUnresolved
            }
            WorkerFailureKind::PromptDelivery => Self::PromptMisdelivery,
            WorkerFailureKind::Protocol => Self::McpHandshakeFailure,
            WorkerFailureKind::Provider | WorkerFailureKind::StartupNoEvidence => {
                Self::ProviderFailure
            }
        }
    }
}

impl std::fmt::Display for FailureScenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustPromptUnresolved => write!(f, "trust_prompt_unresolved"),
            Self::PromptMisdelivery => write!(f, "prompt_misdelivery"),
            Self::StaleBranch => write!(f, "stale_branch"),
            Self::CompileRedCrossCrate => write!(f, "compile_red_cross_crate"),
            Self::McpHandshakeFailure => write!(f, "mcp_handshake_failure"),
            Self::PartialPluginStartup => write!(f, "partial_plugin_startup"),
            Self::ProviderFailure => write!(f, "provider_failure"),
        }
    }
}

/// Individual step that can be executed as part of a recovery recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStep {
    AcceptTrustPrompt,
    RedirectPromptToAgent,
    RebaseBranch,
    CleanBuild,
    RetryMcpHandshake { timeout: u64 },
    RestartPlugin { name: String },
    RestartWorker,
    EscalateToHuman { reason: String },
}

/// Policy governing what happens when automatic recovery is exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationPolicy {
    AlertHuman,
    LogAndContinue,
    Abort,
}

/// A recovery recipe encodes the sequence of steps to attempt for a
/// given failure scenario, along with the maximum number of automatic
/// attempts and the escalation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRecipe {
    pub scenario: FailureScenario,
    pub steps: Vec<RecoveryStep>,
    pub max_attempts: u32,
    pub escalation_policy: EscalationPolicy,
}

/// Outcome of a recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryResult {
    Recovered {
        steps_taken: u32,
    },
    PartialRecovery {
        recovered: Vec<RecoveryStep>,
        remaining: Vec<RecoveryStep>,
    },
    EscalationRequired {
        reason: String,
    },
}

/// Type of recovery execution represented in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAttemptType {
    Automatic,
}

/// Result for one executable recovery command/step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCommandResult {
    pub command: RecoveryStep,
    pub status: RecoveryAttemptState,
    pub result: String,
}

/// Structured event emitted during recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryEvent {
    RecoveryAttempted {
        scenario: FailureScenario,
        recipe: RecoveryRecipe,
        result: RecoveryResult,
    },
    RecoverySucceeded,
    RecoveryFailed,
    Escalated,
}

/// Machine-readable recovery progress for one failure scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryLedgerEntry {
    pub recipe_id: String,
    pub attempt_type: RecoveryAttemptType,
    pub trigger: FailureScenario,
    pub attempt_count: u32,
    pub retry_limit: u32,
    pub attempts_remaining: u32,
    pub state: RecoveryAttemptState,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub command_results: Vec<RecoveryCommandResult>,
    pub result: Option<RecoveryResult>,
    pub last_failure_summary: Option<String>,
    pub escalation_reason: Option<String>,
}

/// Current state of a recovery recipe attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAttemptState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Exhausted,
}

/// Machine-readable status projection for callers that need to
/// distinguish an untouched scenario from an exhausted recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryStatusReport {
    pub scenario: FailureScenario,
    pub attempted: bool,
    pub state: Option<RecoveryAttemptState>,
    pub attempt_count: u32,
    pub retry_limit: Option<u32>,
    pub attempts_remaining: Option<u32>,
    pub escalation_reason: Option<String>,
}

/// Minimal context for tracking recovery state and emitting events.
///
/// Holds per-scenario attempt counts, a structured event log, a recovery
/// attempt ledger, and an optional simulation knob for controlling step
/// outcomes during tests.
#[derive(Debug, Clone, Default)]
pub struct RecoveryContext {
    attempts: HashMap<FailureScenario, u32>,
    events: Vec<RecoveryEvent>,
    ledger: HashMap<FailureScenario, RecoveryLedgerEntry>,
    clock_tick: u64,
    /// Optional step index at which simulated execution fails.
    /// `None` means all steps succeed.
    fail_at_step: Option<usize>,
}

impl RecoveryContext {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure a step index at which simulated execution will fail.
    #[must_use]
    pub fn with_fail_at_step(mut self, index: usize) -> Self {
        self.fail_at_step = Some(index);
        self
    }

    /// Returns the structured event log populated during recovery.
    #[must_use]
    pub fn events(&self) -> &[RecoveryEvent] {
        &self.events
    }

    /// Returns the number of recovery attempts made for a scenario.
    #[must_use]
    pub fn attempt_count(&self, scenario: &FailureScenario) -> u32 {
        self.attempts.get(scenario).copied().unwrap_or(0)
    }

    /// Returns the machine-readable recovery ledger entry for a scenario.
    #[must_use]
    pub fn ledger_entry(&self, scenario: &FailureScenario) -> Option<&RecoveryLedgerEntry> {
        self.ledger.get(scenario)
    }

    /// Returns all recovery ledger entries currently tracked by this context.
    #[must_use]
    pub fn ledger_entries(&self) -> Vec<&RecoveryLedgerEntry> {
        let mut entries: Vec<_> = self.ledger.values().collect();
        entries.sort_by(|left, right| left.recipe_id.cmp(&right.recipe_id));
        entries
    }

    /// Returns a compact machine-readable recovery status for a scenario,
    /// including `attempted = false` when no ledger entry exists yet.
    #[must_use]
    pub fn status_report(&self, scenario: &FailureScenario) -> RecoveryStatusReport {
        self.ledger_entry(scenario).map_or(
            RecoveryStatusReport {
                scenario: *scenario,
                attempted: false,
                state: None,
                attempt_count: 0,
                retry_limit: None,
                attempts_remaining: None,
                escalation_reason: None,
            },
            |entry| RecoveryStatusReport {
                scenario: *scenario,
                attempted: entry.attempt_count > 0,
                state: Some(entry.state),
                attempt_count: entry.attempt_count,
                retry_limit: Some(entry.retry_limit),
                attempts_remaining: Some(entry.attempts_remaining),
                escalation_reason: entry.escalation_reason.clone(),
            },
        )
    }

    fn next_timestamp(&mut self) -> String {
        self.clock_tick += 1;
        format!("recovery-ledger-tick-{}", self.clock_tick)
    }
}

/// Returns the known recovery recipe for the given failure scenario.
#[must_use]
pub fn recipe_for(scenario: &FailureScenario) -> RecoveryRecipe {
    match scenario {
        FailureScenario::TrustPromptUnresolved => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::AcceptTrustPrompt],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::AlertHuman,
        },
        FailureScenario::PromptMisdelivery => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::RedirectPromptToAgent],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::AlertHuman,
        },
        FailureScenario::StaleBranch => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::RebaseBranch, RecoveryStep::CleanBuild],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::AlertHuman,
        },
        FailureScenario::CompileRedCrossCrate => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::CleanBuild],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::AlertHuman,
        },
        FailureScenario::McpHandshakeFailure => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::RetryMcpHandshake { timeout: 5000 }],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::Abort,
        },
        FailureScenario::PartialPluginStartup => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![
                RecoveryStep::RestartPlugin {
                    name: "stalled".to_string(),
                },
                RecoveryStep::RetryMcpHandshake { timeout: 3000 },
            ],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::LogAndContinue,
        },
        FailureScenario::ProviderFailure => RecoveryRecipe {
            scenario: *scenario,
            steps: vec![RecoveryStep::RestartWorker],
            max_attempts: 1,
            escalation_policy: EscalationPolicy::AlertHuman,
        },
    }
}

/// Attempts automatic recovery for the given failure scenario.
///
/// Looks up the recipe, enforces the one-attempt-before-escalation
/// policy, simulates step execution (controlled by the context), and
/// emits structured [`RecoveryEvent`]s for every attempt.
#[allow(clippy::too_many_lines)]
pub fn attempt_recovery(scenario: &FailureScenario, ctx: &mut RecoveryContext) -> RecoveryResult {
    let recipe = recipe_for(scenario);
    let recipe_id = scenario.to_string();
    ctx.ledger
        .entry(*scenario)
        .or_insert_with(|| RecoveryLedgerEntry {
            recipe_id: recipe_id.clone(),
            attempt_type: RecoveryAttemptType::Automatic,
            trigger: *scenario,
            attempt_count: 0,
            retry_limit: recipe.max_attempts,
            attempts_remaining: recipe.max_attempts,
            state: RecoveryAttemptState::Queued,
            started_at: None,
            finished_at: None,
            command_results: Vec::new(),
            result: None,
            last_failure_summary: None,
            escalation_reason: None,
        });

    let current_attempts = ctx.attempt_count(scenario);

    // Enforce one automatic recovery attempt before escalation.
    if current_attempts >= recipe.max_attempts {
        let result = RecoveryResult::EscalationRequired {
            reason: format!(
                "max recovery attempts ({}) exceeded for {}",
                recipe.max_attempts, scenario
            ),
        };
        let finished_at = ctx.next_timestamp();
        if let Some(entry) = ctx.ledger.get_mut(scenario) {
            entry.attempt_count = current_attempts;
            entry.attempts_remaining = 0;
            entry.state = RecoveryAttemptState::Exhausted;
            entry.finished_at = Some(finished_at);
            entry.result = Some(result.clone());
            let RecoveryResult::EscalationRequired { reason } = &result else {
                unreachable!("exhaustion always produces escalation");
            };
            entry.last_failure_summary = Some(reason.clone());
            entry.escalation_reason = Some(reason.clone());
        }
        ctx.events.push(RecoveryEvent::RecoveryAttempted {
            scenario: *scenario,
            recipe,
            result: result.clone(),
        });
        ctx.events.push(RecoveryEvent::Escalated);
        return result;
    }

    let updated_attempts = ctx.attempts.entry(*scenario).or_insert(0);
    *updated_attempts += 1;
    let updated_attempts = *updated_attempts;
    let started_at = ctx.next_timestamp();
    if let Some(entry) = ctx.ledger.get_mut(scenario) {
        entry.attempt_count = updated_attempts;
        entry.attempts_remaining = recipe.max_attempts.saturating_sub(updated_attempts);
        entry.state = RecoveryAttemptState::Running;
        entry.started_at = Some(started_at);
        entry.finished_at = None;
        entry.command_results.clear();
        entry.result = None;
        entry.last_failure_summary = None;
        entry.escalation_reason = None;
    }

    // Execute steps, honoring the optional fail_at_step simulation.
    let fail_index = ctx.fail_at_step;
    let mut executed = Vec::new();
    let mut command_results = Vec::new();
    let mut failed = false;

    for (i, step) in recipe.steps.iter().enumerate() {
        if fail_index == Some(i) {
            command_results.push(RecoveryCommandResult {
                command: step.clone(),
                status: RecoveryAttemptState::Failed,
                result: format!("step {i} failed for {scenario}"),
            });
            failed = true;
            break;
        }
        executed.push(step.clone());
        command_results.push(RecoveryCommandResult {
            command: step.clone(),
            status: RecoveryAttemptState::Succeeded,
            result: format!("step {i} succeeded for {scenario}"),
        });
    }

    let result = if failed {
        let remaining: Vec<RecoveryStep> = recipe.steps[executed.len()..].to_vec();
        if executed.is_empty() {
            RecoveryResult::EscalationRequired {
                reason: format!("recovery failed at first step for {}", scenario),
            }
        } else {
            RecoveryResult::PartialRecovery {
                recovered: executed,
                remaining,
            }
        }
    } else {
        RecoveryResult::Recovered {
            steps_taken: recipe.steps.len() as u32,
        }
    };

    // Emit the attempt as structured event data.
    let finished_at = ctx.next_timestamp();
    if let Some(entry) = ctx.ledger.get_mut(scenario) {
        entry.finished_at = Some(finished_at);
        entry.command_results = command_results;
        entry.result = Some(result.clone());
        match &result {
            RecoveryResult::Recovered { .. } => {
                entry.state = RecoveryAttemptState::Succeeded;
            }
            RecoveryResult::PartialRecovery { remaining, .. } => {
                entry.state = RecoveryAttemptState::Failed;
                entry.last_failure_summary = Some(format!(
                    "{} step(s) remaining after partial recovery",
                    remaining.len()
                ));
            }
            RecoveryResult::EscalationRequired { reason } => {
                entry.state = RecoveryAttemptState::Exhausted;
                entry.last_failure_summary = Some(reason.clone());
                entry.escalation_reason = Some(reason.clone());
            }
        }
    }
    ctx.events.push(RecoveryEvent::RecoveryAttempted {
        scenario: *scenario,
        recipe,
        result: result.clone(),
    });

    match &result {
        RecoveryResult::Recovered { .. } => {
            ctx.events.push(RecoveryEvent::RecoverySucceeded);
        }
        RecoveryResult::PartialRecovery { .. } => {
            ctx.events.push(RecoveryEvent::RecoveryFailed);
        }
        RecoveryResult::EscalationRequired { .. } => {
            ctx.events.push(RecoveryEvent::Escalated);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_scenario_has_a_matching_recipe() {
        // given
        let scenarios = FailureScenario::all();

        // when / then
        for scenario in scenarios {
            let recipe = recipe_for(scenario);
            assert_eq!(
                recipe.scenario, *scenario,
                "recipe scenario should match requested scenario"
            );
            assert!(
                !recipe.steps.is_empty(),
                "recipe for {} should have at least one step",
                scenario
            );
            assert!(
                recipe.max_attempts >= 1,
                "recipe for {} should allow at least one attempt",
                scenario
            );
        }
    }

    #[test]
    fn successful_recovery_returns_recovered_and_emits_events() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::TrustPromptUnresolved;

        // when
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        assert_eq!(result, RecoveryResult::Recovered { steps_taken: 1 });
        assert_eq!(ctx.events().len(), 2);
        assert!(matches!(
            &ctx.events()[0],
            RecoveryEvent::RecoveryAttempted {
                scenario: s,
                result: r,
                ..
            } if *s == FailureScenario::TrustPromptUnresolved
              && matches!(r, RecoveryResult::Recovered { steps_taken: 1 })
        ));
        assert_eq!(ctx.events()[1], RecoveryEvent::RecoverySucceeded);
    }

    #[test]
    fn escalation_after_max_attempts_exceeded() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::PromptMisdelivery;

        // when — first attempt succeeds
        let first = attempt_recovery(&scenario, &mut ctx);
        assert!(matches!(first, RecoveryResult::Recovered { .. }));

        // when — second attempt should escalate
        let second = attempt_recovery(&scenario, &mut ctx);

        // then
        assert!(
            matches!(
                &second,
                RecoveryResult::EscalationRequired { reason }
                    if reason.contains("max recovery attempts")
            ),
            "second attempt should require escalation, got: {second:?}"
        );
        assert_eq!(ctx.attempt_count(&scenario), 1);
        assert!(ctx
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::Escalated)));
    }

    #[test]
    fn partial_recovery_when_step_fails_midway() {
        // given — PartialPluginStartup has two steps; fail at step index 1
        let mut ctx = RecoveryContext::new().with_fail_at_step(1);
        let scenario = FailureScenario::PartialPluginStartup;

        // when
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        match &result {
            RecoveryResult::PartialRecovery {
                recovered,
                remaining,
            } => {
                assert_eq!(recovered.len(), 1, "one step should have succeeded");
                assert_eq!(remaining.len(), 1, "one step should remain");
                assert!(matches!(recovered[0], RecoveryStep::RestartPlugin { .. }));
                assert!(matches!(
                    remaining[0],
                    RecoveryStep::RetryMcpHandshake { .. }
                ));
            }
            other => panic!("expected PartialRecovery, got {other:?}"),
        }
        assert!(ctx
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::RecoveryFailed)));
    }

    #[test]
    fn first_step_failure_escalates_immediately() {
        // given — fail at step index 0
        let mut ctx = RecoveryContext::new().with_fail_at_step(0);
        let scenario = FailureScenario::CompileRedCrossCrate;

        // when
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        assert!(
            matches!(
                &result,
                RecoveryResult::EscalationRequired { reason }
                    if reason.contains("failed at first step")
            ),
            "zero-step failure should escalate, got: {result:?}"
        );
        assert!(ctx
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::Escalated)));
    }

    #[test]
    fn emitted_events_include_structured_attempt_data() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::McpHandshakeFailure;

        // when
        let _ = attempt_recovery(&scenario, &mut ctx);

        // then — verify the RecoveryAttempted event carries full context
        let attempted = ctx
            .events()
            .iter()
            .find(|e| matches!(e, RecoveryEvent::RecoveryAttempted { .. }))
            .expect("should have emitted RecoveryAttempted event");

        match attempted {
            RecoveryEvent::RecoveryAttempted {
                scenario: s,
                recipe,
                result,
            } => {
                assert_eq!(*s, scenario);
                assert_eq!(recipe.scenario, scenario);
                assert!(!recipe.steps.is_empty());
                assert!(matches!(result, RecoveryResult::Recovered { .. }));
            }
            _ => unreachable!(),
        }

        // Verify the event is serializable as structured JSON
        let json = serde_json::to_string(&ctx.events()[0])
            .expect("recovery event should be serializable to JSON");
        assert!(
            json.contains("mcp_handshake_failure"),
            "serialized event should contain scenario name"
        );
    }

    #[test]
    fn recovery_context_tracks_attempts_per_scenario() {
        // given
        let mut ctx = RecoveryContext::new();

        // when
        assert_eq!(ctx.attempt_count(&FailureScenario::StaleBranch), 0);
        attempt_recovery(&FailureScenario::StaleBranch, &mut ctx);

        // then
        assert_eq!(ctx.attempt_count(&FailureScenario::StaleBranch), 1);
        assert_eq!(ctx.attempt_count(&FailureScenario::PromptMisdelivery), 0);
    }

    #[test]
    fn recovery_context_exposes_machine_readable_ledger() {
        // given
        let mut ctx = RecoveryContext::new();

        // when
        let result = attempt_recovery(&FailureScenario::StaleBranch, &mut ctx);

        // then
        assert_eq!(result, RecoveryResult::Recovered { steps_taken: 2 });
        let entry = ctx
            .ledger_entry(&FailureScenario::StaleBranch)
            .expect("stale branch ledger entry");
        assert_eq!(entry.recipe_id, "stale_branch");
        assert_eq!(entry.attempt_type, RecoveryAttemptType::Automatic);
        assert_eq!(entry.trigger, FailureScenario::StaleBranch);
        assert_eq!(entry.attempt_count, 1);
        assert_eq!(entry.retry_limit, 1);
        assert_eq!(entry.attempts_remaining, 0);
        assert_eq!(entry.state, RecoveryAttemptState::Succeeded);
        assert!(entry.started_at.is_some());
        assert!(entry.finished_at.is_some());
        assert_eq!(
            entry.result,
            Some(RecoveryResult::Recovered { steps_taken: 2 })
        );
        assert_eq!(entry.command_results.len(), 2);
        assert_eq!(entry.command_results[0].command, RecoveryStep::RebaseBranch);
        assert_eq!(
            entry.command_results[0].status,
            RecoveryAttemptState::Succeeded
        );
        assert_eq!(entry.last_failure_summary, None);
        assert_eq!(entry.escalation_reason, None);
    }

    #[test]
    fn recovery_ledger_records_exhausted_escalation_reason() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::PromptMisdelivery;

        // when
        let _ = attempt_recovery(&scenario, &mut ctx);
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        assert!(matches!(result, RecoveryResult::EscalationRequired { .. }));
        let entry = ctx.ledger_entry(&scenario).expect("ledger entry");
        assert_eq!(entry.state, RecoveryAttemptState::Exhausted);
        assert_eq!(entry.attempt_count, 1);
        assert_eq!(entry.attempts_remaining, 0);
        assert!(matches!(
            entry.result,
            Some(RecoveryResult::EscalationRequired { .. })
        ));
        assert!(entry
            .escalation_reason
            .as_deref()
            .expect("escalation reason")
            .contains("max recovery attempts"));
    }

    #[test]
    fn recovery_status_report_distinguishes_not_attempted_from_exhausted() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::PromptMisdelivery;

        // then — no ledger entry is not the same as exhausted.
        let not_attempted = ctx.status_report(&scenario);
        assert!(!not_attempted.attempted);
        assert_eq!(not_attempted.state, None);
        assert_eq!(not_attempted.attempt_count, 0);
        assert_eq!(not_attempted.retry_limit, None);

        // when — one allowed attempt then one extra attempt.
        let _ = attempt_recovery(&scenario, &mut ctx);
        let _ = attempt_recovery(&scenario, &mut ctx);

        // then
        let exhausted = ctx.status_report(&scenario);
        assert!(exhausted.attempted);
        assert_eq!(exhausted.state, Some(RecoveryAttemptState::Exhausted));
        assert_eq!(exhausted.attempt_count, 1);
        assert_eq!(exhausted.retry_limit, Some(1));
        assert_eq!(exhausted.attempts_remaining, Some(0));
        assert!(exhausted
            .escalation_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max recovery attempts")));
    }

    #[test]
    fn recovery_ledger_records_failed_command_result() {
        // given
        let mut ctx = RecoveryContext::new().with_fail_at_step(1);
        let scenario = FailureScenario::PartialPluginStartup;

        // when
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        assert!(matches!(result, RecoveryResult::PartialRecovery { .. }));
        let entry = ctx.ledger_entry(&scenario).expect("ledger entry");
        assert_eq!(entry.state, RecoveryAttemptState::Failed);
        assert_eq!(entry.command_results.len(), 2);
        assert_eq!(
            entry.command_results[0].status,
            RecoveryAttemptState::Succeeded
        );
        assert_eq!(
            entry.command_results[1].status,
            RecoveryAttemptState::Failed
        );
        assert!(entry.command_results[1]
            .result
            .contains("partial_plugin_startup"));
    }

    #[test]
    fn stale_branch_recipe_has_rebase_then_clean_build() {
        // given
        let recipe = recipe_for(&FailureScenario::StaleBranch);

        // then
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[0], RecoveryStep::RebaseBranch);
        assert_eq!(recipe.steps[1], RecoveryStep::CleanBuild);
    }

    #[test]
    fn partial_plugin_startup_recipe_has_restart_then_handshake() {
        // given
        let recipe = recipe_for(&FailureScenario::PartialPluginStartup);

        // then
        assert_eq!(recipe.steps.len(), 2);
        assert!(matches!(
            recipe.steps[0],
            RecoveryStep::RestartPlugin { .. }
        ));
        assert!(matches!(
            recipe.steps[1],
            RecoveryStep::RetryMcpHandshake { timeout: 3000 }
        ));
        assert_eq!(recipe.escalation_policy, EscalationPolicy::LogAndContinue);
    }

    #[test]
    fn failure_scenario_display_all_variants() {
        // given
        let cases = [
            (
                FailureScenario::TrustPromptUnresolved,
                "trust_prompt_unresolved",
            ),
            (FailureScenario::PromptMisdelivery, "prompt_misdelivery"),
            (FailureScenario::StaleBranch, "stale_branch"),
            (
                FailureScenario::CompileRedCrossCrate,
                "compile_red_cross_crate",
            ),
            (
                FailureScenario::McpHandshakeFailure,
                "mcp_handshake_failure",
            ),
            (
                FailureScenario::PartialPluginStartup,
                "partial_plugin_startup",
            ),
        ];

        // when / then
        for (scenario, expected) in &cases {
            assert_eq!(scenario.to_string(), *expected);
        }
    }

    #[test]
    fn multi_step_success_reports_correct_steps_taken() {
        // given — StaleBranch has 2 steps, no simulated failure
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::StaleBranch;

        // when
        let result = attempt_recovery(&scenario, &mut ctx);

        // then
        assert_eq!(result, RecoveryResult::Recovered { steps_taken: 2 });
    }

    #[test]
    fn mcp_handshake_recipe_uses_abort_escalation_policy() {
        // given
        let recipe = recipe_for(&FailureScenario::McpHandshakeFailure);

        // then
        assert_eq!(recipe.escalation_policy, EscalationPolicy::Abort);
        assert_eq!(recipe.max_attempts, 1);
    }

    #[test]
    fn worker_failure_kind_maps_to_failure_scenario() {
        // given / when / then — verify the bridge is correct
        assert_eq!(
            FailureScenario::from_worker_failure_kind(WorkerFailureKind::TrustGate),
            FailureScenario::TrustPromptUnresolved,
        );
        assert_eq!(
            FailureScenario::from_worker_failure_kind(WorkerFailureKind::PromptDelivery),
            FailureScenario::PromptMisdelivery,
        );
        assert_eq!(
            FailureScenario::from_worker_failure_kind(WorkerFailureKind::Protocol),
            FailureScenario::McpHandshakeFailure,
        );
        assert_eq!(
            FailureScenario::from_worker_failure_kind(WorkerFailureKind::Provider),
            FailureScenario::ProviderFailure,
        );
    }

    #[test]
    fn provider_failure_recipe_uses_restart_worker_step() {
        // given
        let recipe = recipe_for(&FailureScenario::ProviderFailure);

        // then
        assert_eq!(recipe.scenario, FailureScenario::ProviderFailure);
        assert!(recipe.steps.contains(&RecoveryStep::RestartWorker));
        assert_eq!(recipe.escalation_policy, EscalationPolicy::AlertHuman);
        assert_eq!(recipe.max_attempts, 1);
    }

    #[test]
    fn provider_failure_recovery_attempt_succeeds_then_escalates() {
        // given
        let mut ctx = RecoveryContext::new();
        let scenario = FailureScenario::ProviderFailure;

        // when — first attempt
        let first = attempt_recovery(&scenario, &mut ctx);
        assert!(matches!(first, RecoveryResult::Recovered { .. }));

        // when — second attempt should escalate (max_attempts=1)
        let second = attempt_recovery(&scenario, &mut ctx);
        assert!(matches!(second, RecoveryResult::EscalationRequired { .. }));
        assert!(ctx
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::Escalated)));
    }
}
