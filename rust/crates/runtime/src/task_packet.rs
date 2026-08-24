use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// Task scope resolution for defining the granularity of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskScope {
    /// Work across the entire workspace
    Workspace,
    /// Work within a specific module/crate
    Module,
    /// Work on a single file
    SingleFile,
    /// Custom scope defined by the user
    Custom,
}

impl std::fmt::Display for TaskScope {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace => write!(f, "workspace"),
            Self::Module => write!(f, "module"),
            Self::SingleFile => write!(f, "single-file"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPacket {
    pub objective: String,
    pub scope: TaskScope,
    /// Optional scope path when scope is `Module`, `SingleFile`, or `Custom`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    pub repo: String,
    /// Worktree path for the task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub branch_policy: String,
    /// Legacy verification commands kept for compatibility with existing task packets.
    #[serde(default)]
    pub acceptance_tests: Vec<String>,
    /// Human-readable acceptance criteria for the task objective.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// Files, directories, services, or other resources the task is allowed to touch.
    #[serde(default)]
    pub resources: Vec<TaskResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<String>,
    pub commit_policy: String,
    /// Legacy reporting contract kept for compatibility with existing task packets.
    pub reporting_contract: String,
    #[serde(default)]
    pub reporting_targets: Vec<String>,
    /// Legacy escalation policy kept for compatibility with existing task packets.
    pub escalation_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_policy: Option<String>,
    #[serde(default)]
    pub verification_plan: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResource {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPacketValidationError {
    errors: Vec<String>,
}

impl TaskPacketValidationError {
    #[must_use]
    pub fn new(errors: Vec<String>) -> Self {
        Self { errors }
    }

    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
}

impl Display for TaskPacketValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.errors.join("; "))
    }
}

impl std::error::Error for TaskPacketValidationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPacket(TaskPacket);

impl ValidatedPacket {
    #[must_use]
    pub fn packet(&self) -> &TaskPacket {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> TaskPacket {
        self.0
    }
}

pub fn validate_packet(packet: TaskPacket) -> Result<ValidatedPacket, TaskPacketValidationError> {
    let mut errors = Vec::new();

    validate_required("objective", &packet.objective, &mut errors);
    validate_required("repo", &packet.repo, &mut errors);
    validate_required("branch_policy", &packet.branch_policy, &mut errors);
    validate_required("commit_policy", &packet.commit_policy, &mut errors);
    if packet.reporting_contract.trim().is_empty() && packet.reporting_targets.is_empty() {
        errors.push("reporting_contract or reporting_targets must not be empty".to_string());
    }
    if packet.escalation_policy.trim().is_empty()
        && packet
            .recovery_policy
            .as_ref()
            .is_none_or(|policy| policy.trim().is_empty())
    {
        errors.push("escalation_policy or recovery_policy must not be empty".to_string());
    }

    // Validate scope-specific requirements
    validate_scope_requirements(&packet, &mut errors);

    if packet.acceptance_tests.is_empty() && packet.acceptance_criteria.is_empty() {
        errors.push("acceptance_tests or acceptance_criteria must not be empty".to_string());
    }

    for (index, test) in packet.acceptance_tests.iter().enumerate() {
        if test.trim().is_empty() {
            errors.push(format!(
                "acceptance_tests contains an empty value at index {index}"
            ));
        }
    }

    for (index, criterion) in packet.acceptance_criteria.iter().enumerate() {
        if criterion.trim().is_empty() {
            errors.push(format!(
                "acceptance_criteria contains an empty value at index {index}"
            ));
        }
    }

    for (index, resource) in packet.resources.iter().enumerate() {
        if resource.kind.trim().is_empty() || resource.value.trim().is_empty() {
            errors.push(format!(
                "resources contains an incomplete entry at index {index}"
            ));
        }
    }

    validate_optional("model", packet.model.as_deref(), &mut errors);
    validate_optional("provider", packet.provider.as_deref(), &mut errors);
    validate_optional(
        "permission_profile",
        packet.permission_profile.as_deref(),
        &mut errors,
    );
    validate_optional(
        "recovery_policy",
        packet.recovery_policy.as_deref(),
        &mut errors,
    );

    for (index, step) in packet.verification_plan.iter().enumerate() {
        if step.trim().is_empty() {
            errors.push(format!(
                "verification_plan contains an empty value at index {index}"
            ));
        }
    }

    if errors.is_empty() {
        Ok(ValidatedPacket(packet))
    } else {
        Err(TaskPacketValidationError::new(errors))
    }
}

fn validate_scope_requirements(packet: &TaskPacket, errors: &mut Vec<String>) {
    // Scope path is required for Module, SingleFile, and Custom scopes
    let needs_scope_path = matches!(
        packet.scope,
        TaskScope::Module | TaskScope::SingleFile | TaskScope::Custom
    );

    if needs_scope_path
        && packet
            .scope_path
            .as_ref()
            .is_none_or(|p| p.trim().is_empty())
    {
        errors.push(format!(
            "scope_path is required for scope '{}'",
            packet.scope
        ));
    }
}

fn validate_required(field: &str, value: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        errors.push(format!("{field} must not be empty"));
    }
}

fn validate_optional(field: &str, value: Option<&str>, errors: &mut Vec<String>) {
    if value.is_some_and(|value| value.trim().is_empty()) {
        errors.push(format!("{field} must not be empty when present"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet() -> TaskPacket {
        TaskPacket {
            objective: "Implement typed task packet format".to_string(),
            scope: TaskScope::Module,
            scope_path: Some("runtime/task system".to_string()),
            repo: "claw-code-parity".to_string(),
            worktree: Some("/tmp/wt-1".to_string()),
            branch_policy: "origin/main only".to_string(),
            acceptance_tests: vec![
                "cargo build --workspace".to_string(),
                "cargo test --workspace".to_string(),
            ],
            acceptance_criteria: vec!["packet can launch without pane scraping".to_string()],
            resources: vec![TaskResource {
                kind: "file".to_string(),
                value: "rust/crates/runtime/src/task_packet.rs".to_string(),
            }],
            model: Some("gpt-5.5".to_string()),
            provider: Some("openai".to_string()),
            permission_profile: Some("workspace-write".to_string()),
            commit_policy: "single verified commit".to_string(),
            reporting_contract: "print build result, test result, commit sha".to_string(),
            reporting_targets: vec!["leader".to_string()],
            escalation_policy: "stop only on destructive ambiguity".to_string(),
            recovery_policy: Some("retry once then escalate".to_string()),
            verification_plan: vec!["cargo test -p runtime task_packet".to_string()],
        }
    }

    #[test]
    fn valid_packet_passes_validation() {
        let packet = sample_packet();
        let validated = validate_packet(packet.clone()).expect("packet should validate");
        assert_eq!(validated.packet(), &packet);
        assert_eq!(validated.into_inner(), packet);
    }

    #[test]
    fn invalid_packet_accumulates_errors() {
        use super::TaskScope;
        let packet = TaskPacket {
            objective: " ".to_string(),
            scope: TaskScope::Workspace,
            scope_path: None,
            worktree: None,
            repo: String::new(),
            branch_policy: "\t".to_string(),
            acceptance_tests: vec!["ok".to_string(), " ".to_string()],
            acceptance_criteria: vec![" ".to_string()],
            resources: vec![TaskResource {
                kind: " ".to_string(),
                value: "resource".to_string(),
            }],
            model: Some(" ".to_string()),
            provider: Some("openai".to_string()),
            permission_profile: Some("workspace-write".to_string()),
            commit_policy: String::new(),
            reporting_contract: String::new(),
            reporting_targets: Vec::new(),
            escalation_policy: String::new(),
            recovery_policy: None,
            verification_plan: vec![" ".to_string()],
        };

        let error = validate_packet(packet).expect_err("packet should be rejected");

        assert!(error.errors().len() >= 7);
        assert!(error
            .errors()
            .contains(&"objective must not be empty".to_string()));
        assert!(error
            .errors()
            .contains(&"repo must not be empty".to_string()));
        assert!(error
            .errors()
            .contains(&"acceptance_tests contains an empty value at index 1".to_string()));
    }

    #[test]
    fn legacy_packet_json_deserializes_with_defaulted_cc2_fields() {
        let legacy = r#"{
            "objective": "Legacy packet",
            "scope": "workspace",
            "repo": "claw-code",
            "branch_policy": "origin/main only",
            "acceptance_tests": ["cargo test"],
            "commit_policy": "single commit",
            "reporting_contract": "report sha",
            "escalation_policy": "ask leader"
        }"#;

        let packet: TaskPacket = serde_json::from_str(legacy).expect("legacy packet should load");

        assert_eq!(packet.objective, "Legacy packet");
        assert!(packet.acceptance_criteria.is_empty());
        assert!(packet.resources.is_empty());
        assert_eq!(packet.model, None);
        validate_packet(packet).expect("legacy packet remains valid through aliases");
    }

    #[test]
    fn rich_cc2_packet_fields_roundtrip_and_validate() {
        let packet = sample_packet();
        let json = serde_json::to_value(&packet).expect("packet should serialize");

        assert_eq!(
            json["acceptance_criteria"][0],
            "packet can launch without pane scraping"
        );
        assert_eq!(json["resources"][0]["kind"], "file");
        assert_eq!(json["model"], "gpt-5.5");
        assert_eq!(json["provider"], "openai");
        assert_eq!(json["permission_profile"], "workspace-write");
        assert_eq!(json["recovery_policy"], "retry once then escalate");
        assert_eq!(
            json["verification_plan"][0],
            "cargo test -p runtime task_packet"
        );

        let roundtrip: TaskPacket = serde_json::from_value(json).expect("rich packet roundtrips");
        validate_packet(roundtrip).expect("rich packet validates");
    }

    #[test]
    fn serialization_roundtrip_preserves_packet() {
        let packet = sample_packet();
        let serialized = serde_json::to_string(&packet).expect("packet should serialize");
        let deserialized: TaskPacket =
            serde_json::from_str(&serialized).expect("packet should deserialize");
        assert_eq!(deserialized, packet);
    }

    #[test]
    fn legacy_packet_json_deserializes_with_defaults() {
        let legacy = r#"{
            "objective": "Ship legacy task packet",
            "scope": "module",
            "scope_path": "runtime/task system",
            "repo": "claw-code-parity",
            "worktree": "/tmp/wt-legacy",
            "branch_policy": "origin/main only",
            "acceptance_tests": ["cargo test --workspace"],
            "commit_policy": "single verified commit",
            "reporting_contract": "print build result, test result, commit sha",
            "escalation_policy": "manual escalation"
        }"#;

        let packet: TaskPacket = serde_json::from_str(legacy).expect("legacy packet should parse");

        assert_eq!(packet.acceptance_criteria, Vec::<String>::new());
        assert_eq!(packet.permission_profile, None);
        assert_eq!(packet.model, None);
        assert_eq!(packet.provider, None);
        assert_eq!(packet.recovery_policy, None);
        assert_eq!(packet.commit_policy, "single verified commit");
        assert_eq!(
            packet.reporting_contract,
            "print build result, test result, commit sha"
        );
        assert_eq!(packet.escalation_policy, "manual escalation");
        validate_packet(packet).expect("legacy packet should remain valid");
    }

    #[test]
    fn new_schema_fields_validate_without_legacy_acceptance_tests() {
        let mut packet = sample_packet();
        packet.acceptance_tests.clear();
        packet.reporting_contract.clear();
        packet.escalation_policy.clear();

        validate_packet(packet).expect("new schema fields should be sufficient");
    }

    #[test]
    fn scoped_packets_require_scope_path() {
        for scope in [TaskScope::Module, TaskScope::SingleFile, TaskScope::Custom] {
            let mut packet = sample_packet();
            packet.scope = scope;
            packet.scope_path = Some(" ".to_string());

            let error = validate_packet(packet).expect_err("scoped packet should require path");
            assert!(error
                .errors()
                .contains(&format!("scope_path is required for scope '{scope}'")));
        }
    }

    #[test]
    fn modern_required_groups_report_missing_fallbacks() {
        let mut packet = sample_packet();
        packet.acceptance_criteria.clear();
        packet.acceptance_tests.clear();
        packet.recovery_policy = None;
        packet.escalation_policy.clear();
        packet.reporting_targets.clear();
        packet.reporting_contract.clear();

        let error = validate_packet(packet).expect_err("packet should require task policies");

        for expected in [
            "acceptance_tests or acceptance_criteria must not be empty",
            "escalation_policy or recovery_policy must not be empty",
            "reporting_contract or reporting_targets must not be empty",
        ] {
            assert!(error.errors().contains(&expected.to_string()));
        }
    }

    #[test]
    fn permission_profile_serializes_as_optional_string() {
        let mut packet = sample_packet();
        packet.permission_profile = Some("danger-full-access".to_string());

        let json = serde_json::to_value(&packet).expect("packet should serialize");
        assert_eq!(json["permission_profile"], "danger-full-access");

        let roundtrip: TaskPacket =
            serde_json::from_value(json).expect("packet should deserialize");
        assert_eq!(
            roundtrip.permission_profile.as_deref(),
            Some("danger-full-access")
        );
    }
}
