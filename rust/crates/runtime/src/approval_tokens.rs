use std::collections::BTreeMap;

/// Machine-readable policy exception scope that an approval token may override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalScope {
    pub policy: String,
    pub action: String,
    pub repository: Option<String>,
    pub branch: Option<String>,
}

impl ApprovalScope {
    #[must_use]
    pub fn new(policy: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            policy: policy.into(),
            action: action.into(),
            repository: None,
            branch: None,
        }
    }

    #[must_use]
    pub fn with_repository(mut self, repository: impl Into<String>) -> Self {
        self.repository = Some(repository.into());
        self
    }

    #[must_use]
    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
}

/// Actor/session hop recorded when an approval is delegated or consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDelegationHop {
    pub actor: String,
    pub session_id: Option<String>,
    pub reason: String,
}

impl ApprovalDelegationHop {
    #[must_use]
    pub fn new(actor: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            session_id: None,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Current lifecycle state for a policy-exception approval token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTokenStatus {
    Pending,
    Granted,
    Consumed,
    Expired,
    Revoked,
}

impl ApprovalTokenStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "approval_pending",
            Self::Granted => "approval_granted",
            Self::Consumed => "approval_consumed",
            Self::Expired => "approval_expired",
            Self::Revoked => "approval_revoked",
        }
    }
}

/// Typed policy errors returned when a token cannot authorize a blocked action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalTokenError {
    NoApproval,
    ApprovalPending,
    ApprovalExpired,
    ApprovalRevoked,
    ApprovalAlreadyConsumed,
    ScopeMismatch {
        expected: Box<ApprovalScope>,
        actual: Box<ApprovalScope>,
    },
    UnauthorizedDelegate {
        expected: String,
        actual: String,
    },
}

impl ApprovalTokenError {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoApproval => "no_approval",
            Self::ApprovalPending => "approval_pending",
            Self::ApprovalExpired => "approval_expired",
            Self::ApprovalRevoked => "approval_revoked",
            Self::ApprovalAlreadyConsumed => "approval_already_consumed",
            Self::ScopeMismatch { .. } => "approval_scope_mismatch",
            Self::UnauthorizedDelegate { .. } => "approval_unauthorized_delegate",
        }
    }
}

/// Approval grant bound to a policy/action scope, approving owner, and executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTokenGrant {
    pub token: String,
    pub scope: ApprovalScope,
    pub approving_actor: String,
    pub approved_executor: String,
    pub status: ApprovalTokenStatus,
    pub expires_at_epoch_seconds: Option<u64>,
    pub max_uses: u32,
    pub uses: u32,
    delegation_chain: Vec<ApprovalDelegationHop>,
}

impl ApprovalTokenGrant {
    #[must_use]
    pub fn pending(
        token: impl Into<String>,
        scope: ApprovalScope,
        approving_actor: impl Into<String>,
        approved_executor: impl Into<String>,
    ) -> Self {
        Self {
            token: token.into(),
            scope,
            approving_actor: approving_actor.into(),
            approved_executor: approved_executor.into(),
            status: ApprovalTokenStatus::Pending,
            expires_at_epoch_seconds: None,
            max_uses: 1,
            uses: 0,
            delegation_chain: Vec::new(),
        }
    }

    #[must_use]
    pub fn granted(
        token: impl Into<String>,
        scope: ApprovalScope,
        approving_actor: impl Into<String>,
        approved_executor: impl Into<String>,
    ) -> Self {
        Self::pending(token, scope, approving_actor, approved_executor).approve()
    }

    #[must_use]
    pub fn approve(mut self) -> Self {
        self.status = ApprovalTokenStatus::Granted;
        self
    }

    #[must_use]
    pub fn expires_at(mut self, epoch_seconds: u64) -> Self {
        self.expires_at_epoch_seconds = Some(epoch_seconds);
        self
    }

    #[must_use]
    pub fn with_max_uses(mut self, max_uses: u32) -> Self {
        self.max_uses = max_uses.max(1);
        self
    }

    #[must_use]
    pub fn with_delegation_hop(mut self, hop: ApprovalDelegationHop) -> Self {
        self.delegation_chain.push(hop);
        self
    }

    #[must_use]
    pub fn delegation_chain(&self) -> &[ApprovalDelegationHop] {
        &self.delegation_chain
    }
}

/// Auditable result of verifying or consuming an approval token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTokenAudit {
    pub token: String,
    pub scope: ApprovalScope,
    pub approving_actor: String,
    pub executing_actor: String,
    pub status: ApprovalTokenStatus,
    pub delegated_execution: bool,
    pub delegation_chain: Vec<ApprovalDelegationHop>,
    pub uses: u32,
    pub max_uses: u32,
}

/// In-memory approval-token ledger with one-time-use and replay protection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalTokenLedger {
    grants: BTreeMap<String, ApprovalTokenGrant>,
}

impl ApprovalTokenLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, grant: ApprovalTokenGrant) {
        self.grants.insert(grant.token.clone(), grant);
    }

    #[must_use]
    pub fn get(&self, token: &str) -> Option<&ApprovalTokenGrant> {
        self.grants.get(token)
    }

    pub fn revoke(&mut self, token: &str) -> Result<ApprovalTokenAudit, ApprovalTokenError> {
        let grant = self
            .grants
            .get_mut(token)
            .ok_or(ApprovalTokenError::NoApproval)?;
        grant.status = ApprovalTokenStatus::Revoked;
        Ok(Self::audit_for(grant, &grant.approved_executor))
    }

    pub fn verify(
        &self,
        token: &str,
        scope: &ApprovalScope,
        executing_actor: &str,
        now_epoch_seconds: u64,
    ) -> Result<ApprovalTokenAudit, ApprovalTokenError> {
        let grant = self
            .grants
            .get(token)
            .ok_or(ApprovalTokenError::NoApproval)?;
        Self::validate_grant(grant, scope, executing_actor, now_epoch_seconds)?;
        Ok(Self::audit_for(grant, executing_actor))
    }

    pub fn consume(
        &mut self,
        token: &str,
        scope: &ApprovalScope,
        executing_actor: &str,
        now_epoch_seconds: u64,
    ) -> Result<ApprovalTokenAudit, ApprovalTokenError> {
        let grant = self
            .grants
            .get_mut(token)
            .ok_or(ApprovalTokenError::NoApproval)?;
        Self::validate_grant(grant, scope, executing_actor, now_epoch_seconds)?;
        grant.uses += 1;
        if grant.uses >= grant.max_uses {
            grant.status = ApprovalTokenStatus::Consumed;
        }
        Ok(Self::audit_for(grant, executing_actor))
    }

    fn validate_grant(
        grant: &ApprovalTokenGrant,
        scope: &ApprovalScope,
        executing_actor: &str,
        now_epoch_seconds: u64,
    ) -> Result<(), ApprovalTokenError> {
        match grant.status {
            ApprovalTokenStatus::Pending => return Err(ApprovalTokenError::ApprovalPending),
            ApprovalTokenStatus::Consumed => {
                return Err(ApprovalTokenError::ApprovalAlreadyConsumed)
            }
            ApprovalTokenStatus::Expired => return Err(ApprovalTokenError::ApprovalExpired),
            ApprovalTokenStatus::Revoked => return Err(ApprovalTokenError::ApprovalRevoked),
            ApprovalTokenStatus::Granted => {}
        }

        if grant
            .expires_at_epoch_seconds
            .is_some_and(|expires_at| now_epoch_seconds > expires_at)
        {
            return Err(ApprovalTokenError::ApprovalExpired);
        }

        if grant.uses >= grant.max_uses {
            return Err(ApprovalTokenError::ApprovalAlreadyConsumed);
        }

        if grant.scope != *scope {
            return Err(ApprovalTokenError::ScopeMismatch {
                expected: Box::new(grant.scope.clone()),
                actual: Box::new(scope.clone()),
            });
        }

        if grant.approved_executor != executing_actor {
            return Err(ApprovalTokenError::UnauthorizedDelegate {
                expected: grant.approved_executor.clone(),
                actual: executing_actor.to_string(),
            });
        }

        Ok(())
    }

    fn audit_for(grant: &ApprovalTokenGrant, executing_actor: &str) -> ApprovalTokenAudit {
        let mut delegation_chain = grant.delegation_chain.clone();
        if delegation_chain.is_empty() {
            delegation_chain.push(ApprovalDelegationHop::new(
                grant.approving_actor.clone(),
                "approval granted",
            ));
        }
        if grant.approving_actor != executing_actor
            && !delegation_chain
                .iter()
                .any(|hop| hop.actor == executing_actor)
        {
            delegation_chain.push(ApprovalDelegationHop::new(
                executing_actor.to_string(),
                "delegated execution",
            ));
        }

        ApprovalTokenAudit {
            token: grant.token.clone(),
            scope: grant.scope.clone(),
            approving_actor: grant.approving_actor.clone(),
            executing_actor: executing_actor.to_string(),
            status: grant.status,
            delegated_execution: grant.approving_actor != executing_actor,
            delegation_chain,
            uses: grant.uses,
            max_uses: grant.max_uses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalDelegationHop, ApprovalScope, ApprovalTokenError, ApprovalTokenGrant,
        ApprovalTokenLedger, ApprovalTokenStatus,
    };

    #[test]
    fn approval_token_blocks_until_owner_grants_policy_exception() {
        let mut ledger = ApprovalTokenLedger::new();
        let scope = ApprovalScope::new("main_push_forbidden", "git push")
            .with_repository("sisyphus/claw-code")
            .with_branch("main");
        ledger.insert(ApprovalTokenGrant::pending(
            "tok-pending",
            scope.clone(),
            "repo-owner",
            "release-bot",
        ));

        assert!(matches!(
            ledger.verify("tok-missing", &scope, "release-bot", 10),
            Err(ApprovalTokenError::NoApproval)
        ));
        assert!(matches!(
            ledger.verify("tok-pending", &scope, "release-bot", 10),
            Err(ApprovalTokenError::ApprovalPending)
        ));

        ledger.insert(ApprovalTokenGrant::granted(
            "tok-granted",
            scope.clone(),
            "repo-owner",
            "release-bot",
        ));
        let audit = ledger
            .verify("tok-granted", &scope, "release-bot", 10)
            .expect("owner approval should verify");

        assert_eq!(audit.status, ApprovalTokenStatus::Granted);
        assert_eq!(audit.approving_actor, "repo-owner");
        assert_eq!(audit.executing_actor, "release-bot");
        assert!(audit.delegated_execution);
    }

    #[test]
    fn approval_token_is_one_time_use_and_rejects_replay() {
        let mut ledger = ApprovalTokenLedger::new();
        let scope = ApprovalScope::new("release_requires_owner", "release publish")
            .with_repository("sisyphus/claw-code");
        ledger.insert(ApprovalTokenGrant::granted(
            "tok-once",
            scope.clone(),
            "owner",
            "release-bot",
        ));

        let first = ledger
            .consume("tok-once", &scope, "release-bot", 10)
            .expect("first use should consume token");
        assert_eq!(first.status, ApprovalTokenStatus::Consumed);
        assert_eq!(first.uses, 1);

        assert!(matches!(
            ledger.consume("tok-once", &scope, "release-bot", 11),
            Err(ApprovalTokenError::ApprovalAlreadyConsumed)
        ));
        assert_eq!(
            ledger.get("tok-once").map(|grant| grant.status),
            Some(ApprovalTokenStatus::Consumed)
        );
    }

    #[test]
    fn approval_token_rejects_scope_expansion_expiry_and_revocation() {
        let mut ledger = ApprovalTokenLedger::new();
        let scope = ApprovalScope::new("main_push_forbidden", "git push")
            .with_repository("sisyphus/claw-code")
            .with_branch("main");
        let dev_scope = ApprovalScope::new("main_push_forbidden", "git push")
            .with_repository("sisyphus/claw-code")
            .with_branch("dev");

        ledger.insert(
            ApprovalTokenGrant::granted("tok-expiring", scope.clone(), "owner", "bot")
                .expires_at(20),
        );

        assert!(matches!(
            ledger.verify("tok-expiring", &dev_scope, "bot", 10),
            Err(ApprovalTokenError::ScopeMismatch { .. })
        ));
        assert!(matches!(
            ledger.verify("tok-expiring", &scope, "bot", 21),
            Err(ApprovalTokenError::ApprovalExpired)
        ));

        ledger.insert(ApprovalTokenGrant::granted(
            "tok-revoked",
            scope.clone(),
            "owner",
            "bot",
        ));
        let revoked = ledger
            .revoke("tok-revoked")
            .expect("revocation should be audited");
        assert_eq!(revoked.status, ApprovalTokenStatus::Revoked);
        assert!(matches!(
            ledger.verify("tok-revoked", &scope, "bot", 10),
            Err(ApprovalTokenError::ApprovalRevoked)
        ));
    }

    #[test]
    fn approval_token_preserves_delegation_traceability() {
        let mut ledger = ApprovalTokenLedger::new();
        let scope = ApprovalScope::new("deploy_requires_owner", "deploy prod");
        ledger.insert(
            ApprovalTokenGrant::granted("tok-delegated", scope.clone(), "owner", "deploy-bot")
                .with_delegation_hop(
                    ApprovalDelegationHop::new("owner", "owner approval")
                        .with_session_id("session-owner"),
                )
                .with_delegation_hop(
                    ApprovalDelegationHop::new("lead-agent", "handoff to deploy bot")
                        .with_session_id("session-lead"),
                ),
        );

        assert!(matches!(
            ledger.verify("tok-delegated", &scope, "unexpected-bot", 10),
            Err(ApprovalTokenError::UnauthorizedDelegate { expected, actual })
                if expected == "deploy-bot" && actual == "unexpected-bot"
        ));

        let audit = ledger
            .consume("tok-delegated", &scope, "deploy-bot", 10)
            .expect("approved delegate should consume token");
        let actors = audit
            .delegation_chain
            .iter()
            .map(|hop| hop.actor.as_str())
            .collect::<Vec<_>>();

        assert!(audit.delegated_execution);
        assert_eq!(actors, vec!["owner", "lead-agent", "deploy-bot"]);
        assert_eq!(
            audit.delegation_chain[0].session_id.as_deref(),
            Some("session-owner")
        );
        assert_eq!(
            audit.delegation_chain[1].session_id.as_deref(),
            Some("session-lead")
        );
    }
}
