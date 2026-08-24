use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GreenLevel {
    TargetedTests,
    Package,
    Workspace,
    MergeReady,
}

impl GreenLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TargetedTests => "targeted_tests",
            Self::Package => "package",
            Self::Workspace => "workspace",
            Self::MergeReady => "merge_ready",
        }
    }
}

impl std::fmt::Display for GreenLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GreenContract {
    pub required_level: GreenLevel,
    pub requirements: Vec<GreenContractRequirement>,
    pub block_known_flakes: bool,
}

impl GreenContract {
    #[must_use]
    pub fn new(required_level: GreenLevel) -> Self {
        Self {
            required_level,
            requirements: Vec::new(),
            block_known_flakes: false,
        }
    }

    #[must_use]
    pub fn merge_ready(required_level: GreenLevel) -> Self {
        Self {
            required_level,
            requirements: vec![
                GreenContractRequirement::TestCommandProvenance,
                GreenContractRequirement::BaseBranchFreshness,
                GreenContractRequirement::RecoveryAttemptContext,
            ],
            block_known_flakes: true,
        }
    }

    #[must_use]
    pub fn evaluate(&self, observed_level: Option<GreenLevel>) -> GreenContractOutcome {
        match observed_level {
            Some(level) if level >= self.required_level => GreenContractOutcome::Satisfied {
                required_level: self.required_level,
                observed_level: level,
            },
            _ => GreenContractOutcome::Unsatisfied {
                required_level: self.required_level,
                observed_level,
            },
        }
    }

    #[must_use]
    pub fn evaluate_evidence(&self, evidence: &GreenEvidence) -> GreenEvidenceOutcome {
        let mut missing = Vec::new();
        let mut blocking_flakes = Vec::new();

        if evidence.observed_level < self.required_level {
            missing.push(GreenContractRequirement::RequiredLevel);
        }

        for requirement in &self.requirements {
            match requirement {
                GreenContractRequirement::TestCommandProvenance
                    if !evidence.has_passing_test_command() =>
                {
                    missing.push(*requirement);
                }
                GreenContractRequirement::BaseBranchFreshness if !evidence.base_branch_fresh => {
                    missing.push(*requirement);
                }
                GreenContractRequirement::RecoveryAttemptContext
                    if !evidence.recovery_attempt_context_recorded =>
                {
                    missing.push(*requirement);
                }
                _ => {}
            }
        }

        if self.block_known_flakes {
            blocking_flakes = evidence
                .known_flakes
                .iter()
                .filter(|flake| flake.blocks_green)
                .cloned()
                .collect();
        }

        if missing.is_empty() && blocking_flakes.is_empty() {
            GreenEvidenceOutcome::Satisfied {
                required_level: self.required_level,
                observed_level: evidence.observed_level,
            }
        } else {
            GreenEvidenceOutcome::Unsatisfied {
                required_level: self.required_level,
                observed_level: evidence.observed_level,
                missing,
                blocking_flakes,
            }
        }
    }

    #[must_use]
    pub fn is_satisfied_by(&self, observed_level: GreenLevel) -> bool {
        observed_level >= self.required_level
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GreenEvidence {
    pub observed_level: GreenLevel,
    pub test_commands: Vec<TestCommandProvenance>,
    pub base_branch_fresh: bool,
    pub known_flakes: Vec<KnownFlake>,
    pub recovery_attempt_context_recorded: bool,
}

impl GreenEvidence {
    #[must_use]
    pub fn new(observed_level: GreenLevel) -> Self {
        Self {
            observed_level,
            test_commands: Vec::new(),
            base_branch_fresh: false,
            known_flakes: Vec::new(),
            recovery_attempt_context_recorded: false,
        }
    }

    #[must_use]
    pub fn with_test_command(mut self, command: impl Into<String>, exit_code: i32) -> Self {
        self.test_commands.push(TestCommandProvenance {
            command: command.into(),
            exit_code,
        });
        self
    }

    #[must_use]
    pub fn with_base_branch_fresh(mut self, is_fresh: bool) -> Self {
        self.base_branch_fresh = is_fresh;
        self
    }

    #[must_use]
    pub fn with_known_flake(mut self, test_name: impl Into<String>, blocks_green: bool) -> Self {
        self.known_flakes.push(KnownFlake {
            test_name: test_name.into(),
            blocks_green,
        });
        self
    }

    #[must_use]
    pub fn with_recovery_attempt_context(mut self, recorded: bool) -> Self {
        self.recovery_attempt_context_recorded = recorded;
        self
    }

    #[must_use]
    pub fn has_passing_test_command(&self) -> bool {
        self.test_commands.iter().any(TestCommandProvenance::passed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCommandProvenance {
    pub command: String,
    pub exit_code: i32,
}

impl TestCommandProvenance {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && !self.command.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownFlake {
    pub test_name: String,
    pub blocks_green: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GreenContractRequirement {
    RequiredLevel,
    TestCommandProvenance,
    BaseBranchFreshness,
    RecoveryAttemptContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GreenEvidenceOutcome {
    Satisfied {
        required_level: GreenLevel,
        observed_level: GreenLevel,
    },
    Unsatisfied {
        required_level: GreenLevel,
        observed_level: GreenLevel,
        missing: Vec<GreenContractRequirement>,
        blocking_flakes: Vec<KnownFlake>,
    },
}

impl GreenEvidenceOutcome {
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        matches!(self, Self::Satisfied { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GreenContractOutcome {
    Satisfied {
        required_level: GreenLevel,
        observed_level: GreenLevel,
    },
    Unsatisfied {
        required_level: GreenLevel,
        observed_level: Option<GreenLevel>,
    },
}

impl GreenContractOutcome {
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        matches!(self, Self::Satisfied { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_matching_level_when_evaluating_contract_then_it_is_satisfied() {
        // given
        let contract = GreenContract::new(GreenLevel::Package);

        // when
        let outcome = contract.evaluate(Some(GreenLevel::Package));

        // then
        assert_eq!(
            outcome,
            GreenContractOutcome::Satisfied {
                required_level: GreenLevel::Package,
                observed_level: GreenLevel::Package,
            }
        );
        assert!(outcome.is_satisfied());
    }

    #[test]
    fn given_higher_level_when_checking_requirement_then_it_still_satisfies_contract() {
        // given
        let contract = GreenContract::new(GreenLevel::TargetedTests);

        // when
        let is_satisfied = contract.is_satisfied_by(GreenLevel::Workspace);

        // then
        assert!(is_satisfied);
    }

    #[test]
    fn given_lower_level_when_evaluating_contract_then_it_is_unsatisfied() {
        // given
        let contract = GreenContract::new(GreenLevel::Workspace);

        // when
        let outcome = contract.evaluate(Some(GreenLevel::Package));

        // then
        assert_eq!(
            outcome,
            GreenContractOutcome::Unsatisfied {
                required_level: GreenLevel::Workspace,
                observed_level: Some(GreenLevel::Package),
            }
        );
        assert!(!outcome.is_satisfied());
    }

    #[test]
    fn given_no_green_level_when_evaluating_contract_then_contract_is_unsatisfied() {
        // given
        let contract = GreenContract::new(GreenLevel::MergeReady);

        // when
        let outcome = contract.evaluate(None);

        // then
        assert_eq!(
            outcome,
            GreenContractOutcome::Unsatisfied {
                required_level: GreenLevel::MergeReady,
                observed_level: None,
            }
        );
    }
    #[test]
    fn merge_ready_contract_requires_provenance_beyond_test_level() {
        // given
        let contract = GreenContract::merge_ready(GreenLevel::Workspace);
        let evidence = GreenEvidence::new(GreenLevel::Workspace)
            .with_test_command("cargo test --manifest-path rust/Cargo.toml", 0);

        // when
        let outcome = contract.evaluate_evidence(&evidence);

        // then
        assert_eq!(
            outcome,
            GreenEvidenceOutcome::Unsatisfied {
                required_level: GreenLevel::Workspace,
                observed_level: GreenLevel::Workspace,
                missing: vec![
                    GreenContractRequirement::BaseBranchFreshness,
                    GreenContractRequirement::RecoveryAttemptContext,
                ],
                blocking_flakes: vec![],
            }
        );
        assert!(!outcome.is_satisfied());
    }

    #[test]
    fn merge_ready_contract_accepts_complete_test_provenance_context() {
        // given
        let contract = GreenContract::merge_ready(GreenLevel::Workspace);
        let evidence = GreenEvidence::new(GreenLevel::MergeReady)
            .with_test_command("cargo test --manifest-path rust/Cargo.toml", 0)
            .with_base_branch_fresh(true)
            .with_recovery_attempt_context(true);

        // when
        let outcome = contract.evaluate_evidence(&evidence);

        // then
        assert_eq!(
            outcome,
            GreenEvidenceOutcome::Satisfied {
                required_level: GreenLevel::Workspace,
                observed_level: GreenLevel::MergeReady,
            }
        );
    }

    #[test]
    fn known_blocking_flake_prevents_green_contract_satisfaction() {
        // given
        let contract = GreenContract::merge_ready(GreenLevel::Workspace);
        let evidence = GreenEvidence::new(GreenLevel::MergeReady)
            .with_test_command("cargo test --manifest-path rust/Cargo.toml", 0)
            .with_base_branch_fresh(true)
            .with_recovery_attempt_context(true)
            .with_known_flake(
                "session_lifecycle_prefers_running_process_over_idle_shell",
                true,
            );

        // when
        let outcome = contract.evaluate_evidence(&evidence);

        // then
        assert_eq!(
            outcome,
            GreenEvidenceOutcome::Unsatisfied {
                required_level: GreenLevel::Workspace,
                observed_level: GreenLevel::MergeReady,
                missing: vec![],
                blocking_flakes: vec![KnownFlake {
                    test_name: "session_lifecycle_prefers_running_process_over_idle_shell"
                        .to_string(),
                    blocks_green: true,
                }],
            }
        );
    }
}
