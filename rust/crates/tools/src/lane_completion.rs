//! Lane completion detector — automatically marks lanes as completed when
//! session finishes successfully with green tests and pushed code.
//!
//! This bridges the gap where `LaneContext::completed` was a passive bool
//! that nothing automatically set. Now completion is detected from:
//! - Agent output shows Finished status
//! - No errors/blockers present  
//! - Tests passed (green status)
//! - Code pushed (has output file)

use runtime::{
    evaluate, LaneBlocker, LaneContext, PolicyAction, PolicyCondition, PolicyEngine, PolicyRule,
    ReviewStatus,
};

use crate::AgentOutput;

/// Detects if a lane should be automatically marked as completed.
///
/// Returns `Some(LaneContext)` with `completed = true` if all conditions met,
/// `None` if lane should remain active.
#[allow(dead_code)]
pub(crate) fn detect_lane_completion(
    output: &AgentOutput,
    test_green: bool,
    has_pushed: bool,
) -> Option<LaneContext> {
    // Must be finished without errors
    if output.error.is_some() {
        return None;
    }

    // Must have finished status
    if !output.status.eq_ignore_ascii_case("completed")
        && !output.status.eq_ignore_ascii_case("finished")
    {
        return None;
    }

    // Must have no current blocker
    if output.current_blocker.is_some() {
        return None;
    }

    // Must have green tests
    if !test_green {
        return None;
    }

    // Must have pushed code
    if !has_pushed {
        return None;
    }

    // All conditions met — create completed context
    Some(LaneContext {
        lane_id: output.agent_id.clone(),
        green_level: 3, // Workspace green
        branch_freshness: std::time::Duration::from_secs(0),
        blocker: LaneBlocker::None,
        review_status: ReviewStatus::Approved,
        diff_scope: runtime::DiffScope::Scoped,
        completed: true,
        reconciled: false,
    })
}

/// Evaluates policy actions for a completed lane.
#[allow(dead_code)]
pub(crate) fn evaluate_completed_lane(context: &LaneContext) -> Vec<PolicyAction> {
    let engine = PolicyEngine::new(vec![
        PolicyRule::new(
            "closeout-completed-lane",
            PolicyCondition::And(vec![
                PolicyCondition::LaneCompleted,
                PolicyCondition::GreenAt { level: 3 },
            ]),
            PolicyAction::CloseoutLane,
            10,
        ),
        PolicyRule::new(
            "cleanup-completed-session",
            PolicyCondition::LaneCompleted,
            PolicyAction::CleanupSession,
            5,
        ),
    ]);

    evaluate(&engine, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::{DiffScope, LaneBlocker};

    fn test_output() -> AgentOutput {
        AgentOutput {
            agent_id: "test-lane-1".to_string(),
            name: "Test Agent".to_string(),
            description: "Test".to_string(),
            subagent_type: None,
            model: None,
            status: "Finished".to_string(),
            output_file: "/tmp/test.output".to_string(),
            manifest_file: "/tmp/test.manifest".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            started_at: Some("2024-01-01T00:00:00Z".to_string()),
            completed_at: Some("2024-01-01T00:00:00Z".to_string()),
            lane_events: vec![],
            derived_state: "working".to_string(),
            current_blocker: None,
            team_id: None,
            task_id: None,
            error: None,
        }
    }

    #[test]
    fn detects_completion_when_all_conditions_met() {
        let output = test_output();
        let result = detect_lane_completion(&output, true, true);

        assert!(result.is_some());
        let context = result.unwrap();
        assert!(context.completed);
        assert_eq!(context.green_level, 3);
        assert_eq!(context.blocker, LaneBlocker::None);
    }

    #[test]
    fn no_completion_when_error_present() {
        let mut output = test_output();
        output.error = Some("Build failed".to_string());

        let result = detect_lane_completion(&output, true, true);
        assert!(result.is_none());
    }

    #[test]
    fn no_completion_when_not_finished() {
        let mut output = test_output();
        output.status = "Running".to_string();

        let result = detect_lane_completion(&output, true, true);
        assert!(result.is_none());
    }

    #[test]
    fn no_completion_when_tests_not_green() {
        let output = test_output();

        let result = detect_lane_completion(&output, false, true);
        assert!(result.is_none());
    }

    #[test]
    fn no_completion_when_not_pushed() {
        let output = test_output();

        let result = detect_lane_completion(&output, true, false);
        assert!(result.is_none());
    }

    #[test]
    fn evaluate_triggers_closeout_for_completed_lane() {
        let context = LaneContext {
            lane_id: "completed-lane".to_string(),
            green_level: 3,
            branch_freshness: std::time::Duration::from_secs(0),
            blocker: LaneBlocker::None,
            review_status: ReviewStatus::Approved,
            diff_scope: DiffScope::Scoped,
            completed: true,
            reconciled: false,
        };

        let actions = evaluate_completed_lane(&context);

        assert!(actions.contains(&PolicyAction::CloseoutLane));
        assert!(actions.contains(&PolicyAction::CleanupSession));
    }
}
