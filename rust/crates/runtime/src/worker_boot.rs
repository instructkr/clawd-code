#![allow(
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::map_unwrap_or
)]
//! In-memory worker-boot state machine and control registry.
//!
//! This provides a foundational control plane for reliable worker startup:
//! trust-gate detection, ready-for-prompt handshakes, and prompt-misdelivery
//! detection/recovery all live above raw terminal transport.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Spawning,
    TrustRequired,
    ToolPermissionRequired,
    ReadyForPrompt,
    Running,
    Finished,
    Failed,
}

impl std::fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawning => write!(f, "spawning"),
            Self::TrustRequired => write!(f, "trust_required"),
            Self::ToolPermissionRequired => write!(f, "tool_permission_required"),
            Self::ReadyForPrompt => write!(f, "ready_for_prompt"),
            Self::Running => write!(f, "running"),
            Self::Finished => write!(f, "finished"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureKind {
    TrustGate,
    ToolPermissionGate,
    PromptDelivery,
    Protocol,
    Provider,
    StartupNoEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerFailure {
    pub kind: WorkerFailureKind,
    pub message: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEventKind {
    Spawning,
    StartupPreflightWarning,
    TrustRequired,
    ToolPermissionRequired,
    TrustResolved,
    ReadyForPrompt,
    PromptMisdelivery,
    PromptReplayArmed,
    Running,
    Restarted,
    Finished,
    Failed,
    StartupNoEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerTrustResolution {
    AutoAllowlisted,
    ManualApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPromptTarget {
    Shell,
    WrongTarget,
    WrongTask,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStartupPreflightWarningKind {
    FileAbsentOnBranch,
    GitMetadataNotWritable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerStartupPreflightWarning {
    pub kind: WorkerStartupPreflightWarningKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Classification of startup failure when no evidence is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupFailureClassification {
    /// Trust prompt is required but not detected/resolved
    TrustRequired,
    /// Tool permission prompt is required before startup can continue
    ToolPermissionRequired,
    /// Prompt was delivered to wrong target (shell misdelivery)
    PromptMisdelivery,
    /// Prompt was sent but acceptance timed out
    PromptAcceptanceTimeout,
    /// Transport layer is dead/unresponsive
    TransportDead,
    /// Worker process crashed during startup
    WorkerCrashed,
    /// Cannot determine specific cause
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupHealthSummary {
    /// Whether this subsystem appeared healthy at timeout.
    pub healthy: bool,
    /// Stable placeholder/source string until deeper transport and MCP probes are wired in.
    pub summary: String,
}

impl StartupHealthSummary {
    fn observed(name: &str, healthy: bool) -> Self {
        let status = if healthy { "healthy" } else { "unhealthy" };
        Self {
            healthy,
            summary: format!("{name}_{status}_placeholder"),
        }
    }
}

/// Evidence bundle collected when worker startup times out without clear evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupEvidenceBundle {
    /// Last known worker lifecycle state before timeout
    pub last_lifecycle_state: WorkerStatus,
    /// Timestamp of the last lifecycle state transition, unix epoch seconds
    pub last_lifecycle_at: u64,
    /// The pane/command that was being executed
    pub pane_command: String,
    /// Timestamp when the pane/command snapshot was observed, unix epoch seconds
    pub pane_observed_at: u64,
    /// Timestamp when the worker command was started, unix epoch seconds
    pub command_started_at: u64,
    /// Timestamp when prompt was sent (if any), unix epoch seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_sent_at: Option<u64>,
    /// Whether prompt acceptance was detected
    pub prompt_acceptance_state: bool,
    /// Result of trust prompt detection at timeout
    pub trust_prompt_detected: bool,
    /// Result of tool permission prompt detection at timeout
    pub tool_permission_prompt_detected: bool,
    /// Age in seconds of the latest tool permission prompt, when observed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_permission_prompt_age_seconds: Option<u64>,
    /// Whether the prompt surface exposed only a session allow path or also an always-allow path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_permission_allow_scope: Option<ToolPermissionAllowScope>,
    /// Transport health summary (true = healthy/responsive)
    pub transport_healthy: bool,
    /// Typed transport health placeholder for future concrete probes
    pub transport_health: StartupHealthSummary,
    /// MCP health summary (true = all servers healthy)
    pub mcp_healthy: bool,
    /// Typed MCP health placeholder for future concrete probes
    pub mcp_health: StartupHealthSummary,
    /// Seconds since worker creation
    pub elapsed_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerEventPayload {
    TrustPrompt {
        cwd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolution: Option<WorkerTrustResolution>,
    },
    ToolPermissionPrompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        prompt_age_seconds: u64,
        allow_scope: ToolPermissionAllowScope,
        prompt_preview: String,
    },
    PromptDelivery {
        prompt_preview: String,
        observed_target: WorkerPromptTarget,
        #[serde(skip_serializing_if = "Option::is_none")]
        observed_cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        observed_prompt_preview: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_receipt: Option<WorkerTaskReceipt>,
        recovery_armed: bool,
    },
    StartupNoEvidence {
        evidence: StartupEvidenceBundle,
        classification: StartupFailureClassification,
    },
    StartupPreflightWarning {
        kind: WorkerStartupPreflightWarningKind,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionAllowScope {
    SessionOnly,
    SessionOrAlways,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerTaskReceipt {
    pub repo: String,
    pub task_kind: String,
    pub source_surface: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    pub objective_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerEvent {
    pub seq: u64,
    pub kind: WorkerEventKind,
    pub status: WorkerStatus,
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<WorkerEventPayload>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Worker {
    pub worker_id: String,
    pub cwd: String,
    pub status: WorkerStatus,
    pub trust_auto_resolve: bool,
    pub trust_gate_cleared: bool,
    pub auto_recover_prompt_misdelivery: bool,
    pub prompt_delivery_attempts: u32,
    pub prompt_in_flight: bool,
    pub prompt_sent_at: Option<u64>,
    pub last_prompt: Option<String>,
    pub expected_receipt: Option<WorkerTaskReceipt>,
    pub replay_prompt: Option<String>,
    pub last_error: Option<WorkerFailure>,
    pub created_at: u64,
    pub updated_at: u64,
    pub events: Vec<WorkerEvent>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerRegistry {
    inner: Arc<Mutex<WorkerRegistryInner>>,
}

#[derive(Debug, Default)]
struct WorkerRegistryInner {
    workers: HashMap<String, Worker>,
    counter: u64,
}

impl WorkerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn create(
        &self,
        cwd: &str,
        trusted_roots: &[String],
        auto_recover_prompt_misdelivery: bool,
    ) -> Worker {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        inner.counter += 1;
        let ts = now_secs();
        let worker_id = format!("worker_{:08x}_{}", ts, inner.counter);
        let trust_auto_resolve = trusted_roots
            .iter()
            .any(|root| path_matches_allowlist(cwd, root));
        let mut worker = Worker {
            worker_id: worker_id.clone(),
            cwd: cwd.to_owned(),
            status: WorkerStatus::Spawning,
            trust_auto_resolve,
            trust_gate_cleared: false,
            auto_recover_prompt_misdelivery,
            prompt_delivery_attempts: 0,
            prompt_in_flight: false,
            prompt_sent_at: None,
            last_prompt: None,
            expected_receipt: None,
            replay_prompt: None,
            last_error: None,
            created_at: ts,
            updated_at: ts,
            events: Vec::new(),
        };
        push_event(
            &mut worker,
            WorkerEventKind::Spawning,
            WorkerStatus::Spawning,
            Some("worker created".to_string()),
            None,
        );
        inner.workers.insert(worker_id, worker.clone());
        worker
    }

    #[must_use]
    pub fn get(&self, worker_id: &str) -> Option<Worker> {
        let inner = self.inner.lock().expect("worker registry lock poisoned");
        inner.workers.get(worker_id).cloned()
    }

    pub fn observe_startup_preflight(
        &self,
        worker_id: &str,
        task_prompt: &str,
    ) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        for warning in startup_preflight_warnings(Path::new(&worker.cwd), task_prompt) {
            push_event(
                worker,
                WorkerEventKind::StartupPreflightWarning,
                worker.status,
                Some(warning.message.clone()),
                Some(WorkerEventPayload::StartupPreflightWarning {
                    kind: warning.kind,
                    message: warning.message,
                    path: warning.path,
                }),
            );
        }

        Ok(worker.clone())
    }

    pub fn observe(&self, worker_id: &str, screen_text: &str) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;
        let lowered = screen_text.to_ascii_lowercase();

        if let Some(tool_prompt) = detect_tool_permission_prompt(screen_text, &lowered) {
            worker.status = WorkerStatus::ToolPermissionRequired;
            worker.last_error = Some(WorkerFailure {
                kind: WorkerFailureKind::ToolPermissionGate,
                message: tool_prompt.message(),
                created_at: now_secs(),
            });
            push_event(
                worker,
                WorkerEventKind::ToolPermissionRequired,
                WorkerStatus::ToolPermissionRequired,
                Some("tool permission prompt detected".to_string()),
                Some(WorkerEventPayload::ToolPermissionPrompt {
                    server_name: tool_prompt.server_name,
                    tool_name: tool_prompt.tool_name,
                    prompt_age_seconds: 0,
                    allow_scope: tool_prompt.allow_scope,
                    prompt_preview: tool_prompt.prompt_preview,
                }),
            );
            return Ok(worker.clone());
        }

        if !worker.trust_gate_cleared && detect_trust_prompt(&lowered) {
            worker.status = WorkerStatus::TrustRequired;
            worker.last_error = Some(WorkerFailure {
                kind: WorkerFailureKind::TrustGate,
                message: "worker boot blocked on trust prompt".to_string(),
                created_at: now_secs(),
            });
            push_event(
                worker,
                WorkerEventKind::TrustRequired,
                WorkerStatus::TrustRequired,
                Some("trust prompt detected".to_string()),
                Some(WorkerEventPayload::TrustPrompt {
                    cwd: worker.cwd.clone(),
                    resolution: None,
                }),
            );

            if worker.trust_auto_resolve {
                worker.trust_gate_cleared = true;
                worker.last_error = None;
                worker.status = WorkerStatus::Spawning;
                push_event(
                    worker,
                    WorkerEventKind::TrustResolved,
                    WorkerStatus::Spawning,
                    Some("allowlisted repo auto-resolved trust prompt".to_string()),
                    Some(WorkerEventPayload::TrustPrompt {
                        cwd: worker.cwd.clone(),
                        resolution: Some(WorkerTrustResolution::AutoAllowlisted),
                    }),
                );
            } else {
                return Ok(worker.clone());
            }
        }

        if let Some(observation) = prompt_misdelivery_is_relevant(worker)
            .then(|| {
                detect_prompt_misdelivery(
                    screen_text,
                    &lowered,
                    worker.last_prompt.as_deref(),
                    &worker.cwd,
                    worker.expected_receipt.as_ref(),
                )
            })
            .flatten()
        {
            let prompt_preview = prompt_preview(worker.last_prompt.as_deref().unwrap_or_default());
            let message = match observation.target {
                WorkerPromptTarget::Shell => {
                    format!(
                        "worker prompt landed in shell instead of coding agent: {prompt_preview}"
                    )
                }
                WorkerPromptTarget::WrongTarget => format!(
                    "worker prompt landed in the wrong target instead of {}: {}",
                    worker.cwd, prompt_preview
                ),
                WorkerPromptTarget::WrongTask => format!(
                    "worker prompt receipt mismatched the expected task context for {}: {}",
                    worker.cwd, prompt_preview
                ),
                WorkerPromptTarget::Unknown => format!(
                    "worker prompt delivery failed before reaching coding agent: {prompt_preview}"
                ),
            };
            worker.last_error = Some(WorkerFailure {
                kind: WorkerFailureKind::PromptDelivery,
                message,
                created_at: now_secs(),
            });
            worker.prompt_in_flight = false;
            push_event(
                worker,
                WorkerEventKind::PromptMisdelivery,
                WorkerStatus::Failed,
                Some(prompt_misdelivery_detail(&observation).to_string()),
                Some(WorkerEventPayload::PromptDelivery {
                    prompt_preview: prompt_preview.clone(),
                    observed_target: observation.target,
                    observed_cwd: observation.observed_cwd.clone(),
                    observed_prompt_preview: observation.observed_prompt_preview.clone(),
                    task_receipt: worker.expected_receipt.clone(),
                    recovery_armed: false,
                }),
            );
            if worker.auto_recover_prompt_misdelivery {
                worker.replay_prompt = worker.last_prompt.clone();
                worker.status = WorkerStatus::ReadyForPrompt;
                push_event(
                    worker,
                    WorkerEventKind::PromptReplayArmed,
                    WorkerStatus::ReadyForPrompt,
                    Some("prompt replay armed after prompt misdelivery".to_string()),
                    Some(WorkerEventPayload::PromptDelivery {
                        prompt_preview,
                        observed_target: observation.target,
                        observed_cwd: observation.observed_cwd,
                        observed_prompt_preview: observation.observed_prompt_preview,
                        task_receipt: worker.expected_receipt.clone(),
                        recovery_armed: true,
                    }),
                );
            } else {
                worker.status = WorkerStatus::Failed;
            }
            return Ok(worker.clone());
        }

        if detect_running_cue(&lowered) && worker.prompt_in_flight {
            worker.prompt_in_flight = false;
            worker.status = WorkerStatus::Running;
            worker.last_error = None;
        }

        if detect_ready_for_prompt(screen_text, &lowered)
            && worker.status != WorkerStatus::ReadyForPrompt
        {
            worker.status = WorkerStatus::ReadyForPrompt;
            worker.prompt_in_flight = false;
            if matches!(
                worker.last_error.as_ref().map(|failure| failure.kind),
                Some(WorkerFailureKind::TrustGate)
            ) {
                worker.last_error = None;
            }
            push_event(
                worker,
                WorkerEventKind::ReadyForPrompt,
                WorkerStatus::ReadyForPrompt,
                Some("worker is ready for prompt delivery".to_string()),
                None,
            );
        }

        Ok(worker.clone())
    }

    pub fn resolve_trust(&self, worker_id: &str) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        if worker.status != WorkerStatus::TrustRequired {
            return Err(format!(
                "worker {worker_id} is not waiting on trust; current status: {}",
                worker.status
            ));
        }

        worker.trust_gate_cleared = true;
        worker.last_error = None;
        worker.status = WorkerStatus::Spawning;
        push_event(
            worker,
            WorkerEventKind::TrustResolved,
            WorkerStatus::Spawning,
            Some("trust prompt resolved manually".to_string()),
            Some(WorkerEventPayload::TrustPrompt {
                cwd: worker.cwd.clone(),
                resolution: Some(WorkerTrustResolution::ManualApproval),
            }),
        );
        Ok(worker.clone())
    }

    pub fn send_prompt(
        &self,
        worker_id: &str,
        prompt: Option<&str>,
        task_receipt: Option<WorkerTaskReceipt>,
    ) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        if worker.status != WorkerStatus::ReadyForPrompt {
            return Err(format!(
                "worker {worker_id} is not ready for prompt delivery; current status: {}",
                worker.status
            ));
        }

        let next_prompt = prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| worker.replay_prompt.clone())
            .ok_or_else(|| format!("worker {worker_id} has no prompt to send or replay"))?;

        worker.prompt_delivery_attempts += 1;
        worker.prompt_in_flight = true;
        worker.prompt_sent_at = Some(now_secs());
        worker.last_prompt = Some(next_prompt.clone());
        worker.expected_receipt = task_receipt;
        worker.replay_prompt = None;
        worker.last_error = None;
        worker.status = WorkerStatus::Running;
        push_event(
            worker,
            WorkerEventKind::Running,
            WorkerStatus::Running,
            Some(format!(
                "prompt dispatched to worker: {}",
                prompt_preview(&next_prompt)
            )),
            None,
        );
        Ok(worker.clone())
    }

    pub fn await_ready(&self, worker_id: &str) -> Result<WorkerReadySnapshot, String> {
        let worker = self
            .get(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        Ok(WorkerReadySnapshot {
            worker_id: worker.worker_id.clone(),
            status: worker.status,
            ready: worker.status == WorkerStatus::ReadyForPrompt,
            blocked: matches!(
                worker.status,
                WorkerStatus::TrustRequired
                    | WorkerStatus::ToolPermissionRequired
                    | WorkerStatus::Failed
            ),
            replay_prompt_ready: worker.replay_prompt.is_some(),
            last_error: worker.last_error.clone(),
        })
    }

    pub fn restart(&self, worker_id: &str) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;
        worker.status = WorkerStatus::Spawning;
        worker.trust_gate_cleared = false;
        worker.last_prompt = None;
        worker.replay_prompt = None;
        worker.last_error = None;
        worker.prompt_delivery_attempts = 0;
        worker.prompt_in_flight = false;
        worker.prompt_sent_at = None;
        push_event(
            worker,
            WorkerEventKind::Restarted,
            WorkerStatus::Spawning,
            Some("worker restarted".to_string()),
            None,
        );
        Ok(worker.clone())
    }

    pub fn terminate(&self, worker_id: &str) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;
        worker.status = WorkerStatus::Finished;
        worker.prompt_in_flight = false;
        push_event(
            worker,
            WorkerEventKind::Finished,
            WorkerStatus::Finished,
            Some("worker terminated by control plane".to_string()),
            None,
        );
        Ok(worker.clone())
    }

    /// Classify session completion and transition worker to appropriate terminal state.
    /// Detects degraded completions (finish="unknown" with zero tokens) as provider failures.
    pub fn observe_completion(
        &self,
        worker_id: &str,
        finish_reason: &str,
        tokens_output: u64,
    ) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        let is_provider_failure =
            (finish_reason == "unknown" && tokens_output == 0) || finish_reason == "error";

        if is_provider_failure {
            let message = if finish_reason == "unknown" && tokens_output == 0 {
                "session completed with finish='unknown' and zero output — provider degraded or context exhausted".to_string()
            } else {
                format!("session failed with finish='{finish_reason}' — provider error")
            };

            worker.last_error = Some(WorkerFailure {
                kind: WorkerFailureKind::Provider,
                message,
                created_at: now_secs(),
            });
            worker.status = WorkerStatus::Failed;
            worker.prompt_in_flight = false;
            push_event(
                worker,
                WorkerEventKind::Failed,
                WorkerStatus::Failed,
                Some("provider failure classified".to_string()),
                None,
            );
        } else {
            worker.status = WorkerStatus::Finished;
            worker.prompt_in_flight = false;
            worker.last_error = None;
            push_event(
                worker,
                WorkerEventKind::Finished,
                WorkerStatus::Finished,
                Some(format!(
                    "session completed: finish='{finish_reason}', tokens={tokens_output}"
                )),
                None,
            );
        }

        Ok(worker.clone())
    }

    /// Handle startup timeout by emitting typed `worker.startup_no_evidence` event with evidence bundle.
    /// Classifier attempts to down-rank the vague bucket into a specific failure classification.
    pub fn observe_startup_timeout(
        &self,
        worker_id: &str,
        pane_command: &str,
        transport_healthy: bool,
        mcp_healthy: bool,
    ) -> Result<Worker, String> {
        let mut inner = self.inner.lock().expect("worker registry lock poisoned");
        let worker = inner
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("worker not found: {worker_id}"))?;

        let now = now_secs();
        let elapsed = now.saturating_sub(worker.created_at);
        let latest_tool_permission_event = worker
            .events
            .iter()
            .rev()
            .find(|event| event.kind == WorkerEventKind::ToolPermissionRequired);
        let tool_permission_allow_scope =
            latest_tool_permission_event.and_then(|event| match &event.payload {
                Some(WorkerEventPayload::ToolPermissionPrompt { allow_scope, .. }) => {
                    Some(*allow_scope)
                }
                _ => None,
            });

        // Build evidence bundle
        let evidence = StartupEvidenceBundle {
            last_lifecycle_state: worker.status,
            last_lifecycle_at: worker.updated_at,
            pane_command: pane_command.to_string(),
            pane_observed_at: now,
            command_started_at: worker.created_at,
            prompt_sent_at: worker.prompt_sent_at,
            prompt_acceptance_state: worker.status == WorkerStatus::Running
                && !worker.prompt_in_flight,
            trust_prompt_detected: worker
                .events
                .iter()
                .any(|e| e.kind == WorkerEventKind::TrustRequired),
            tool_permission_prompt_detected: worker
                .events
                .iter()
                .any(|e| e.kind == WorkerEventKind::ToolPermissionRequired),
            tool_permission_prompt_age_seconds: latest_tool_permission_event
                .map(|event| now.saturating_sub(event.timestamp)),
            tool_permission_allow_scope,
            transport_healthy,
            transport_health: StartupHealthSummary::observed("transport", transport_healthy),
            mcp_healthy,
            mcp_health: StartupHealthSummary::observed("mcp", mcp_healthy),
            elapsed_seconds: elapsed,
        };

        // Classify the failure
        let classification = classify_startup_failure(&evidence);

        // Emit failure with evidence
        worker.last_error = Some(WorkerFailure {
            kind: WorkerFailureKind::StartupNoEvidence,
            message: format!(
                "worker startup stalled after {elapsed}s — classified as {classification:?}"
            ),
            created_at: now,
        });
        worker.status = WorkerStatus::Failed;
        worker.prompt_in_flight = false;

        push_event(
            worker,
            WorkerEventKind::StartupNoEvidence,
            WorkerStatus::Failed,
            Some(format!(
                "startup timeout with evidence: last_state={:?}, trust_detected={}, prompt_accepted={}",
                evidence.last_lifecycle_state,
                evidence.trust_prompt_detected,
                evidence.prompt_acceptance_state
            )),
            Some(WorkerEventPayload::StartupNoEvidence {
                evidence,
                classification,
            }),
        );

        Ok(worker.clone())
    }
}

/// Classify startup failure based on evidence bundle.
/// Attempts to down-rank the vague `startup-no-evidence` bucket into a specific failure class.
fn classify_startup_failure(evidence: &StartupEvidenceBundle) -> StartupFailureClassification {
    // Check for transport death first
    if !evidence.transport_healthy {
        return StartupFailureClassification::TransportDead;
    }

    // Check for trust prompt that wasn't resolved
    if evidence.trust_prompt_detected
        && evidence.last_lifecycle_state == WorkerStatus::TrustRequired
    {
        return StartupFailureClassification::TrustRequired;
    }

    // Check for tool permission prompts that were not resolved
    if evidence.tool_permission_prompt_detected
        && evidence.last_lifecycle_state == WorkerStatus::ToolPermissionRequired
    {
        return StartupFailureClassification::ToolPermissionRequired;
    }

    // Check for prompt acceptance timeout
    if evidence.prompt_sent_at.is_some()
        && !evidence.prompt_acceptance_state
        && evidence.last_lifecycle_state == WorkerStatus::Running
    {
        return StartupFailureClassification::PromptAcceptanceTimeout;
    }

    // Check for misdelivery when prompt was sent but not accepted
    if evidence.prompt_sent_at.is_some()
        && !evidence.prompt_acceptance_state
        && evidence.elapsed_seconds > 30
    {
        return StartupFailureClassification::PromptMisdelivery;
    }

    // If MCP is unhealthy but transport is fine, worker may have crashed
    if !evidence.mcp_healthy && evidence.transport_healthy {
        return StartupFailureClassification::WorkerCrashed;
    }

    // Default to unknown if no stronger classification exists
    StartupFailureClassification::Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerReadySnapshot {
    pub worker_id: String,
    pub status: WorkerStatus,
    pub ready: bool,
    pub blocked: bool,
    pub replay_prompt_ready: bool,
    pub last_error: Option<WorkerFailure>,
}

fn prompt_misdelivery_is_relevant(worker: &Worker) -> bool {
    worker.prompt_in_flight && worker.last_prompt.is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptDeliveryObservation {
    target: WorkerPromptTarget,
    observed_cwd: Option<String>,
    observed_prompt_preview: Option<String>,
}

fn push_event(
    worker: &mut Worker,
    kind: WorkerEventKind,
    status: WorkerStatus,
    detail: Option<String>,
    payload: Option<WorkerEventPayload>,
) {
    let timestamp = now_secs();
    let seq = worker.events.len() as u64 + 1;
    worker.updated_at = timestamp;
    worker.status = status;
    worker.events.push(WorkerEvent {
        seq,
        kind,
        status,
        detail,
        payload,
        timestamp,
    });
    emit_state_file(worker);
}

/// Write current worker state to `.claw/worker-state.json` under the worker's cwd.
/// This is the file-based observability surface: external observers (clawhip, orchestrators)
/// poll this file instead of requiring an HTTP route on the opencode binary.
#[derive(serde::Serialize)]
struct StateSnapshot<'a> {
    worker_id: &'a str,
    status: WorkerStatus,
    is_ready: bool,
    trust_gate_cleared: bool,
    prompt_in_flight: bool,
    last_event: Option<&'a WorkerEvent>,
    updated_at: u64,
    /// Seconds since last state transition. Clawhip uses this to detect
    /// stalled workers without computing epoch deltas.
    seconds_since_update: u64,
}

fn emit_state_file(worker: &Worker) {
    let state_dir = std::path::Path::new(&worker.cwd).join(".claw");
    if std::fs::create_dir_all(&state_dir).is_err() {
        return;
    }
    let state_path = state_dir.join("worker-state.json");
    let tmp_path = state_dir.join("worker-state.json.tmp");

    let now = now_secs();
    let snapshot = StateSnapshot {
        worker_id: &worker.worker_id,
        status: worker.status,
        is_ready: worker.status == WorkerStatus::ReadyForPrompt,
        trust_gate_cleared: worker.trust_gate_cleared,
        prompt_in_flight: worker.prompt_in_flight,
        last_event: worker.events.last(),
        updated_at: worker.updated_at,
        seconds_since_update: now.saturating_sub(worker.updated_at),
    };

    if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
        let _ = std::fs::write(&tmp_path, json);
        let _ = std::fs::rename(&tmp_path, &state_path);
    }
}

fn path_matches_allowlist(cwd: &str, trusted_root: &str) -> bool {
    let cwd = normalize_path(cwd);
    let trusted_root = normalize_path(trusted_root);
    cwd == trusted_root || cwd.starts_with(&trusted_root)
}

fn normalize_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolPermissionPromptObservation {
    server_name: Option<String>,
    tool_name: Option<String>,
    allow_scope: ToolPermissionAllowScope,
    prompt_preview: String,
}

impl ToolPermissionPromptObservation {
    fn message(&self) -> String {
        match (&self.server_name, &self.tool_name) {
            (Some(server), Some(tool)) => {
                format!("worker boot blocked on tool permission prompt for {server}.{tool}")
            }
            (Some(server), None) => {
                format!("worker boot blocked on tool permission prompt for {server}")
            }
            (None, Some(tool)) => {
                format!("worker boot blocked on tool permission prompt for {tool}")
            }
            (None, None) => "worker boot blocked on tool permission prompt".to_string(),
        }
    }
}

fn detect_tool_permission_prompt(
    screen_text: &str,
    lowered: &str,
) -> Option<ToolPermissionPromptObservation> {
    let looks_like_prompt = lowered.contains("allow the")
        && lowered.contains("server")
        && lowered.contains("tool")
        && lowered.contains("run");
    let looks_like_tool_gate = lowered.contains("allow tool") && lowered.contains("run");
    if !looks_like_prompt && !looks_like_tool_gate {
        return None;
    }

    let prompt_line = screen_text
        .lines()
        .rev()
        .find(|line| {
            let lowered_line = line.to_ascii_lowercase();
            lowered_line.contains("allow")
                && lowered_line.contains("tool")
                && (lowered_line.contains("run") || lowered_line.contains("server"))
        })
        .unwrap_or(screen_text)
        .trim();

    let tool_name = extract_quoted_value(prompt_line)
        .or_else(|| extract_after(prompt_line, "tool ").map(|token| normalize_tool_token(&token)));
    let server_name = extract_between(prompt_line, "the ", " server")
        .map(|server| server.trim_end_matches(" MCP").to_string())
        .or_else(|| {
            tool_name
                .as_deref()
                .and_then(extract_server_from_qualified_tool)
        });

    Some(ToolPermissionPromptObservation {
        server_name,
        tool_name,
        allow_scope: detect_tool_permission_allow_scope(lowered),
        prompt_preview: prompt_preview(prompt_line),
    })
}

fn detect_tool_permission_allow_scope(lowered: &str) -> ToolPermissionAllowScope {
    let always_allow_capable = [
        "always allow",
        "allow always",
        "allow this tool always",
        "allow for all sessions",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));

    if always_allow_capable {
        return ToolPermissionAllowScope::SessionOrAlways;
    }

    let session_allow_capable = [
        "allow once",
        "allow for this session",
        "allow this session",
        "yes, allow",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));

    if session_allow_capable {
        ToolPermissionAllowScope::SessionOnly
    } else {
        ToolPermissionAllowScope::Unknown
    }
}

fn extract_quoted_value(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_between(text: &str, prefix: &str, suffix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let end = rest.find(suffix)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn extract_after(text: &str, prefix: &str) -> Option<String> {
    let start = text.to_ascii_lowercase().find(prefix)? + prefix.len();
    let value = text[start..]
        .split_whitespace()
        .next()?
        .trim_matches(|ch: char| ch == '?' || ch == ':' || ch == '"' || ch == '\'');
    (!value.is_empty()).then(|| value.to_string())
}

fn normalize_tool_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| ch == '?' || ch == ':' || ch == '"' || ch == '\'')
        .to_string()
}

fn extract_server_from_qualified_tool(tool: &str) -> Option<String> {
    let rest = tool.strip_prefix("mcp__")?;
    let (server, _) = rest.split_once("__")?;
    (!server.is_empty()).then(|| server.to_string())
}

pub fn startup_preflight_warnings(
    cwd: &Path,
    task_prompt: &str,
) -> Vec<WorkerStartupPreflightWarning> {
    let mut warnings = Vec::new();

    if let Some(git_path) = git_metadata_path(cwd) {
        if !path_is_writable(&git_path) {
            warnings.push(WorkerStartupPreflightWarning {
                kind: WorkerStartupPreflightWarningKind::GitMetadataNotWritable,
                message: format!(
                    "git metadata is not writable; commits or pushes may fail: {}",
                    git_path.display()
                ),
                path: Some(git_path.display().to_string()),
            });
        }
    }

    for path in mentioned_repo_paths(task_prompt) {
        if !git_tracks_path(cwd, &path) {
            warnings.push(WorkerStartupPreflightWarning {
                kind: WorkerStartupPreflightWarningKind::FileAbsentOnBranch,
                message: format!(
                    "task mentions {path}, but git does not track it on the current branch"
                ),
                path: Some(path),
            });
        }
    }

    warnings
}

fn mentioned_repo_paths(task_prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in task_prompt.split_whitespace() {
        let token = raw.trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
            )
        });
        if !token.contains('/') || token.contains("://") || token.starts_with('/') {
            continue;
        }
        let token = token.trim_start_matches("./");
        if token.contains("..") {
            continue;
        }
        if token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.'))
            && token
                .rsplit('/')
                .next()
                .is_some_and(|name| name.contains('.'))
            && !out.iter().any(|seen| seen == token)
        {
            out.push(token.to_string());
        }
    }
    out
}

fn git_tracks_path(cwd: &Path, path: &str) -> bool {
    Command::new("git")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(path)
        .current_dir(cwd)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_metadata_path(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    Some(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn path_is_writable(path: &Path) -> bool {
    let probe_dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    std::fs::metadata(probe_dir)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .is_some_and(|metadata| metadata_allows_directory_writes(&metadata))
}

#[cfg(unix)]
fn metadata_allows_directory_writes(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    mode & 0o222 != 0 && mode & 0o111 != 0
}

#[cfg(not(unix))]
fn metadata_allows_directory_writes(metadata: &std::fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

fn detect_trust_prompt(lowered: &str) -> bool {
    [
        "do you trust the files in this folder",
        "trust the files in this folder",
        "trust this folder",
        "allow and continue",
        "yes, proceed",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn detect_ready_for_prompt(screen_text: &str, lowered: &str) -> bool {
    if [
        "ready for input",
        "ready for your input",
        "ready for prompt",
        "send a message",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return true;
    }

    let Some(last_non_empty) = screen_text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
    else {
        return false;
    };
    let trimmed = last_non_empty.trim();
    if is_shell_prompt(trimmed) {
        return false;
    }

    trimmed == ">"
        || trimmed == "›"
        || trimmed == "❯"
        || trimmed.starts_with("> ")
        || trimmed.starts_with("› ")
        || trimmed.starts_with("❯ ")
        || trimmed.contains("│ >")
        || trimmed.contains("│ ›")
        || trimmed.contains("│ ❯")
}

fn detect_running_cue(lowered: &str) -> bool {
    [
        "thinking",
        "working",
        "running tests",
        "inspecting",
        "analyzing",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn is_shell_prompt(trimmed: &str) -> bool {
    trimmed.ends_with('$')
        || trimmed.ends_with('%')
        || trimmed.ends_with('#')
        || trimmed.starts_with('$')
        || trimmed.starts_with('%')
        || trimmed.starts_with('#')
}

fn detect_prompt_misdelivery(
    screen_text: &str,
    lowered: &str,
    prompt: Option<&str>,
    expected_cwd: &str,
    expected_receipt: Option<&WorkerTaskReceipt>,
) -> Option<PromptDeliveryObservation> {
    let Some(prompt) = prompt else {
        return None;
    };

    let prompt_snippet = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if prompt_snippet.is_empty() {
        return None;
    }
    let prompt_visible = lowered.contains(&prompt_snippet);
    let observed_prompt_preview = detect_prompt_echo(screen_text);

    if let Some(receipt) = expected_receipt {
        let receipt_visible = task_receipt_visible(lowered, receipt);
        let mismatched_prompt_visible = observed_prompt_preview
            .as_deref()
            .map(str::to_ascii_lowercase)
            .is_some_and(|preview| !preview.contains(&prompt_snippet));

        if (prompt_visible || mismatched_prompt_visible) && !receipt_visible {
            return Some(PromptDeliveryObservation {
                target: WorkerPromptTarget::WrongTask,
                observed_cwd: detect_observed_shell_cwd(screen_text),
                observed_prompt_preview,
            });
        }
    }

    if let Some(observed_cwd) = detect_observed_shell_cwd(screen_text) {
        if prompt_visible && !cwd_matches_observed_target(expected_cwd, &observed_cwd) {
            return Some(PromptDeliveryObservation {
                target: WorkerPromptTarget::WrongTarget,
                observed_cwd: Some(observed_cwd),
                observed_prompt_preview,
            });
        }
    }

    let shell_error = [
        "command not found",
        "syntax error near unexpected token",
        "parse error near",
        "no such file or directory",
        "unknown command",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));

    (shell_error && prompt_visible).then_some(PromptDeliveryObservation {
        target: WorkerPromptTarget::Shell,
        observed_cwd: None,
        observed_prompt_preview,
    })
}

fn prompt_preview(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= 48 {
        return trimmed.to_string();
    }
    let preview = trimmed.chars().take(48).collect::<String>();
    format!("{}…", preview.trim_end())
}

fn detect_prompt_echo(screen_text: &str) -> Option<String> {
    screen_text.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix('›')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn task_receipt_visible(lowered_screen_text: &str, receipt: &WorkerTaskReceipt) -> bool {
    let expected_tokens = [
        receipt.repo.to_ascii_lowercase(),
        receipt.task_kind.to_ascii_lowercase(),
        receipt.source_surface.to_ascii_lowercase(),
        receipt.objective_preview.to_ascii_lowercase(),
    ];

    expected_tokens
        .iter()
        .all(|token| lowered_screen_text.contains(token))
        && receipt
            .expected_artifacts
            .iter()
            .all(|artifact| lowered_screen_text.contains(&artifact.to_ascii_lowercase()))
}

fn prompt_misdelivery_detail(observation: &PromptDeliveryObservation) -> &'static str {
    match observation.target {
        WorkerPromptTarget::Shell => "shell misdelivery detected",
        WorkerPromptTarget::WrongTarget => "prompt landed in wrong target",
        WorkerPromptTarget::WrongTask => "prompt receipt mismatched expected task context",
        WorkerPromptTarget::Unknown => "prompt delivery failure detected",
    }
}

fn detect_observed_shell_cwd(screen_text: &str) -> Option<String> {
    screen_text.lines().find_map(|line| {
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        tokens
            .iter()
            .position(|token| is_shell_prompt_token(token))
            .and_then(|index| index.checked_sub(1).map(|cwd_index| tokens[cwd_index]))
            .filter(|candidate| looks_like_cwd_label(candidate))
            .map(ToOwned::to_owned)
    })
}

fn is_shell_prompt_token(token: &&str) -> bool {
    matches!(*token, "$" | "%" | "#" | ">" | "›" | "❯")
}

fn looks_like_cwd_label(candidate: &str) -> bool {
    candidate.starts_with('/')
        || candidate.starts_with('~')
        || candidate.starts_with('.')
        || candidate.contains('/')
}

fn cwd_matches_observed_target(expected_cwd: &str, observed_cwd: &str) -> bool {
    let expected = normalize_path(expected_cwd);
    let expected_base = expected
        .file_name()
        .map(|segment| segment.to_string_lossy().into_owned())
        .unwrap_or_else(|| expected.to_string_lossy().into_owned());
    let observed_base = Path::new(observed_cwd)
        .file_name()
        .map(|segment| segment.to_string_lossy().into_owned())
        .unwrap_or_else(|| observed_cwd.trim_matches(':').to_string());

    expected.to_string_lossy().ends_with(observed_cwd)
        || observed_cwd.ends_with(expected.to_string_lossy().as_ref())
        || expected_base == observed_base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn allowlisted_trust_prompt_auto_resolves_then_reaches_ready_state() {
        let registry = WorkerRegistry::new();
        let worker = registry.create(
            "/tmp/worktrees/repo-a",
            &["/tmp/worktrees".to_string()],
            true,
        );

        let after_trust = registry
            .observe(
                &worker.worker_id,
                "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
            )
            .expect("trust observe should succeed");
        assert_eq!(after_trust.status, WorkerStatus::Spawning);
        assert!(after_trust.trust_gate_cleared);
        let trust_required = after_trust
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::TrustRequired)
            .expect("trust required event should exist");
        assert_eq!(
            trust_required.payload,
            Some(WorkerEventPayload::TrustPrompt {
                cwd: "/tmp/worktrees/repo-a".to_string(),
                resolution: None,
            })
        );
        let trust_resolved = after_trust
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::TrustResolved)
            .expect("trust resolved event should exist");
        assert_eq!(
            trust_resolved.payload,
            Some(WorkerEventPayload::TrustPrompt {
                cwd: "/tmp/worktrees/repo-a".to_string(),
                resolution: Some(WorkerTrustResolution::AutoAllowlisted),
            })
        );

        let ready = registry
            .observe(&worker.worker_id, "Ready for your input\n>")
            .expect("ready observe should succeed");
        assert_eq!(ready.status, WorkerStatus::ReadyForPrompt);
        assert!(ready.last_error.is_none());
    }

    #[test]
    fn trust_prompt_blocks_non_allowlisted_worker_until_resolved() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-b", &[], true);

        let blocked = registry
            .observe(
                &worker.worker_id,
                "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
            )
            .expect("trust observe should succeed");
        assert_eq!(blocked.status, WorkerStatus::TrustRequired);
        assert_eq!(
            blocked.last_error.expect("trust error should exist").kind,
            WorkerFailureKind::TrustGate
        );

        let send_before_resolve = registry.send_prompt(&worker.worker_id, Some("ship it"), None);
        assert!(send_before_resolve
            .expect_err("prompt delivery should be gated")
            .contains("not ready for prompt delivery"));

        let resolved = registry
            .resolve_trust(&worker.worker_id)
            .expect("manual trust resolution should succeed");
        assert_eq!(resolved.status, WorkerStatus::Spawning);
        assert!(resolved.trust_gate_cleared);
        let trust_resolved = resolved
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::TrustResolved)
            .expect("manual trust resolve event should exist");
        assert_eq!(
            trust_resolved.payload,
            Some(WorkerEventPayload::TrustPrompt {
                cwd: "/tmp/repo-b".to_string(),
                resolution: Some(WorkerTrustResolution::ManualApproval),
            })
        );
    }

    #[test]
    fn ready_detection_ignores_plain_shell_prompts() {
        assert!(!detect_ready_for_prompt("bellman@host %", "bellman@host %"));
        assert!(!detect_ready_for_prompt("/tmp/repo $", "/tmp/repo $"));
        assert!(detect_ready_for_prompt("│ >", "│ >"));
    }

    #[test]
    fn tool_permission_prompt_blocks_worker_with_structured_event() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-mcp", &[], true);

        let blocked = registry
            .observe(
                &worker.worker_id,
                "Allow the omx_memory MCP server to run tool \"project_memory_read\"?\n\
                 1. Yes, allow once\n\
                 2. Always allow this tool",
            )
            .expect("tool permission observe should succeed");

        assert_eq!(blocked.status, WorkerStatus::ToolPermissionRequired);
        assert_eq!(
            blocked
                .last_error
                .as_ref()
                .expect("tool permission error should exist")
                .kind,
            WorkerFailureKind::ToolPermissionGate
        );
        let event = blocked
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::ToolPermissionRequired)
            .expect("tool permission event should exist");
        assert_eq!(
            event.payload,
            Some(WorkerEventPayload::ToolPermissionPrompt {
                server_name: Some("omx_memory".to_string()),
                tool_name: Some("project_memory_read".to_string()),
                prompt_age_seconds: 0,
                allow_scope: ToolPermissionAllowScope::SessionOrAlways,
                prompt_preview: prompt_preview(
                    "Allow the omx_memory MCP server to run tool \"project_memory_read\"?",
                ),
            })
        );

        let readiness = registry
            .await_ready(&worker.worker_id)
            .expect("ready snapshot should load");
        assert!(readiness.blocked);
        assert!(!readiness.ready);
    }

    #[test]
    fn startup_preflight_warns_when_task_file_is_absent_on_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        Command::new("git")
            .arg("init")
            .current_dir(tmp.path())
            .output()
            .expect("git init should run");
        fs::create_dir_all(tmp.path().join("src")).expect("src dir");
        fs::write(tmp.path().join("src/lib.rs"), "pub fn present() {}\n").expect("write file");
        Command::new("git")
            .args(["add", "src/lib.rs"])
            .current_dir(tmp.path())
            .output()
            .expect("git add should run");

        let warnings = startup_preflight_warnings(
            tmp.path(),
            "Fix src/lib.rs and rust/crates/runtime/src/trident.rs before testing.",
        );

        assert!(warnings.iter().any(|warning| {
            warning.kind == WorkerStartupPreflightWarningKind::FileAbsentOnBranch
                && warning.path.as_deref() == Some("rust/crates/runtime/src/trident.rs")
        }));
        assert!(!warnings.iter().any(|warning| {
            warning.kind == WorkerStartupPreflightWarningKind::FileAbsentOnBranch
                && warning.path.as_deref() == Some("src/lib.rs")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn startup_preflight_warns_when_git_metadata_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let worktree = tmp.path().join("worktree");
        fs::create_dir_all(&worktree).expect("worktree dir");
        Command::new("git")
            .arg("init")
            .current_dir(&worktree)
            .output()
            .expect("git init should run");
        let git_dir = worktree.join(".git");

        let original_permissions = fs::metadata(&git_dir)
            .expect("gitdir metadata")
            .permissions();
        let mut read_only_permissions = original_permissions.clone();
        read_only_permissions.set_mode(0o555);
        fs::set_permissions(&git_dir, read_only_permissions).expect("make gitdir read-only");

        let warnings = startup_preflight_warnings(&worktree, "Audit repository.");
        let registry = WorkerRegistry::new();
        let worker = registry.create(&worktree.display().to_string(), &[], true);
        let observed = registry
            .observe_startup_preflight(&worker.worker_id, "Audit repository.")
            .expect("preflight should run");

        fs::set_permissions(&git_dir, original_permissions).expect("restore gitdir permissions");

        assert!(warnings.iter().any(|warning| {
            warning.kind == WorkerStartupPreflightWarningKind::GitMetadataNotWritable
                && warning.path.as_deref() == Some(git_dir.to_string_lossy().as_ref())
        }));
        assert!(observed.events.iter().any(|event| {
            matches!(
                &event.payload,
                Some(WorkerEventPayload::StartupPreflightWarning {
                    kind: WorkerStartupPreflightWarningKind::GitMetadataNotWritable,
                    path: Some(path),
                    ..
                }) if path == git_dir.to_string_lossy().as_ref()
            )
        }));
    }

    #[test]
    fn startup_preflight_records_structured_warning_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        Command::new("git")
            .arg("init")
            .current_dir(tmp.path())
            .output()
            .expect("git init should run");
        let registry = WorkerRegistry::new();
        let worker = registry.create(&tmp.path().display().to_string(), &[], true);

        let observed = registry
            .observe_startup_preflight(&worker.worker_id, "Open missing/file.rs")
            .expect("preflight should run");

        let event = observed
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::StartupPreflightWarning)
            .expect("preflight warning event");
        assert!(matches!(
            event.payload,
            Some(WorkerEventPayload::StartupPreflightWarning {
                kind: WorkerStartupPreflightWarningKind::FileAbsentOnBranch,
                ..
            })
        ));
    }

    #[test]
    fn startup_timeout_classifies_tool_permission_prompt() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-mcp-timeout", &[], true);

        registry
            .observe(
                &worker.worker_id,
                "Allow the omx_memory MCP server to run tool \"notepad_read\"?\n\
                 1. Yes, allow once",
            )
            .expect("tool permission observe should succeed");

        let timed_out = registry
            .observe_startup_timeout(&worker.worker_id, "claw prompt", true, true)
            .expect("startup timeout observe should succeed");
        let event = timed_out
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::StartupNoEvidence)
            .expect("startup no evidence event should exist");

        match event.payload.as_ref() {
            Some(WorkerEventPayload::StartupNoEvidence {
                classification,
                evidence,
            }) => {
                assert_eq!(
                    *classification,
                    StartupFailureClassification::ToolPermissionRequired
                );
                assert!(evidence.tool_permission_prompt_detected);
                assert_eq!(
                    evidence.tool_permission_allow_scope,
                    Some(ToolPermissionAllowScope::SessionOnly)
                );
                assert!(evidence.tool_permission_prompt_age_seconds.is_some());
            }
            _ => panic!("expected StartupNoEvidence payload"),
        }
    }

    #[test]
    fn prompt_misdelivery_is_detected_and_replay_can_be_rearmed() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-c", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");

        let running = registry
            .send_prompt(&worker.worker_id, Some("Implement worker handshake"), None)
            .expect("prompt send should succeed");
        assert_eq!(running.status, WorkerStatus::Running);
        assert_eq!(running.prompt_delivery_attempts, 1);
        assert!(running.prompt_in_flight);

        let recovered = registry
            .observe(
                &worker.worker_id,
                "% Implement worker handshake\nzsh: command not found: Implement",
            )
            .expect("misdelivery observe should succeed");
        assert_eq!(recovered.status, WorkerStatus::ReadyForPrompt);
        assert_eq!(
            recovered
                .last_error
                .expect("misdelivery error should exist")
                .kind,
            WorkerFailureKind::PromptDelivery
        );
        assert_eq!(
            recovered.replay_prompt.as_deref(),
            Some("Implement worker handshake")
        );
        let misdelivery = recovered
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::PromptMisdelivery)
            .expect("misdelivery event should exist");
        assert_eq!(misdelivery.status, WorkerStatus::Failed);
        assert_eq!(
            misdelivery.payload,
            Some(WorkerEventPayload::PromptDelivery {
                prompt_preview: "Implement worker handshake".to_string(),
                observed_target: WorkerPromptTarget::Shell,
                observed_cwd: None,
                observed_prompt_preview: None,
                task_receipt: None,
                recovery_armed: false,
            })
        );
        let replay = recovered
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::PromptReplayArmed)
            .expect("replay event should exist");
        assert_eq!(replay.status, WorkerStatus::ReadyForPrompt);
        assert_eq!(
            replay.payload,
            Some(WorkerEventPayload::PromptDelivery {
                prompt_preview: "Implement worker handshake".to_string(),
                observed_target: WorkerPromptTarget::Shell,
                observed_cwd: None,
                observed_prompt_preview: None,
                task_receipt: None,
                recovery_armed: true,
            })
        );

        let replayed = registry
            .send_prompt(&worker.worker_id, None, None)
            .expect("replay send should succeed");
        assert_eq!(replayed.status, WorkerStatus::Running);
        assert!(replayed.replay_prompt.is_none());
        assert_eq!(replayed.prompt_delivery_attempts, 2);
    }

    #[test]
    fn prompt_delivery_detects_wrong_target_and_replays_to_expected_worker() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-target-a", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        registry
            .send_prompt(
                &worker.worker_id,
                Some("Run the worker bootstrap tests"),
                None,
            )
            .expect("prompt send should succeed");

        let recovered = registry
            .observe(
                &worker.worker_id,
                "/tmp/repo-target-b % Run the worker bootstrap tests\nzsh: command not found: Run",
            )
            .expect("wrong target should be detected");

        assert_eq!(recovered.status, WorkerStatus::ReadyForPrompt);
        assert_eq!(
            recovered.replay_prompt.as_deref(),
            Some("Run the worker bootstrap tests")
        );
        assert!(recovered
            .last_error
            .expect("wrong target error should exist")
            .message
            .contains("wrong target"));
        let misdelivery = recovered
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::PromptMisdelivery)
            .expect("wrong-target event should exist");
        assert_eq!(
            misdelivery.payload,
            Some(WorkerEventPayload::PromptDelivery {
                prompt_preview: "Run the worker bootstrap tests".to_string(),
                observed_target: WorkerPromptTarget::WrongTarget,
                observed_cwd: Some("/tmp/repo-target-b".to_string()),
                observed_prompt_preview: None,
                task_receipt: None,
                recovery_armed: false,
            })
        );
    }

    #[test]
    fn await_ready_surfaces_blocked_or_ready_worker_state() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-d", &[], false);

        let initial = registry
            .await_ready(&worker.worker_id)
            .expect("await should succeed");
        assert!(!initial.ready);
        assert!(!initial.blocked);

        registry
            .observe(
                &worker.worker_id,
                "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
            )
            .expect("trust observe should succeed");
        let blocked = registry
            .await_ready(&worker.worker_id)
            .expect("await should succeed");
        assert!(!blocked.ready);
        assert!(blocked.blocked);

        registry
            .resolve_trust(&worker.worker_id)
            .expect("manual trust resolution should succeed");
        registry
            .observe(&worker.worker_id, "Ready for your input\n>")
            .expect("ready observe should succeed");
        let ready = registry
            .await_ready(&worker.worker_id)
            .expect("await should succeed");
        assert!(ready.ready);
        assert!(!ready.blocked);
        assert!(ready.last_error.is_none());
    }

    #[test]
    fn wrong_task_receipt_mismatch_is_detected_before_execution_continues() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-task", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        registry
            .send_prompt(
                &worker.worker_id,
                Some("Implement worker handshake"),
                Some(WorkerTaskReceipt {
                    repo: "claw-code".to_string(),
                    task_kind: "repo_code".to_string(),
                    source_surface: "omx_team".to_string(),
                    expected_artifacts: vec!["patch".to_string(), "tests".to_string()],
                    objective_preview: "Implement worker handshake".to_string(),
                }),
            )
            .expect("prompt send should succeed");

        let recovered = registry
            .observe(
                &worker.worker_id,
                "› Explain this KakaoTalk screenshot for a friend\nI can help analyze the screenshot…",
            )
            .expect("mismatch observe should succeed");

        assert_eq!(recovered.status, WorkerStatus::ReadyForPrompt);
        assert_eq!(
            recovered
                .last_error
                .expect("mismatch error should exist")
                .kind,
            WorkerFailureKind::PromptDelivery
        );
        let mismatch = recovered
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::PromptMisdelivery)
            .expect("wrong-task event should exist");
        assert_eq!(mismatch.status, WorkerStatus::Failed);
        assert_eq!(
            mismatch.payload,
            Some(WorkerEventPayload::PromptDelivery {
                prompt_preview: "Implement worker handshake".to_string(),
                observed_target: WorkerPromptTarget::WrongTask,
                observed_cwd: None,
                observed_prompt_preview: Some(
                    "Explain this KakaoTalk screenshot for a friend".to_string()
                ),
                task_receipt: Some(WorkerTaskReceipt {
                    repo: "claw-code".to_string(),
                    task_kind: "repo_code".to_string(),
                    source_surface: "omx_team".to_string(),
                    expected_artifacts: vec!["patch".to_string(), "tests".to_string()],
                    objective_preview: "Implement worker handshake".to_string(),
                }),
                recovery_armed: false,
            })
        );
        let replay = recovered
            .events
            .iter()
            .find(|event| event.kind == WorkerEventKind::PromptReplayArmed)
            .expect("replay event should exist");
        assert_eq!(replay.status, WorkerStatus::ReadyForPrompt);
    }

    #[test]
    fn restart_and_terminate_reset_or_finish_worker() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-e", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        registry
            .send_prompt(&worker.worker_id, Some("Run tests"), None)
            .expect("prompt send should succeed");

        let restarted = registry
            .restart(&worker.worker_id)
            .expect("restart should succeed");
        assert_eq!(restarted.status, WorkerStatus::Spawning);
        assert_eq!(restarted.prompt_delivery_attempts, 0);
        assert!(restarted.last_prompt.is_none());
        assert!(!restarted.prompt_in_flight);

        let finished = registry
            .terminate(&worker.worker_id)
            .expect("terminate should succeed");
        assert_eq!(finished.status, WorkerStatus::Finished);
        assert!(finished
            .events
            .iter()
            .any(|event| event.kind == WorkerEventKind::Finished));
    }

    #[test]
    fn observe_completion_classifies_provider_failure_on_unknown_finish_zero_tokens() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-f", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        registry
            .send_prompt(&worker.worker_id, Some("Run tests"), None)
            .expect("prompt send should succeed");

        let failed = registry
            .observe_completion(&worker.worker_id, "unknown", 0)
            .expect("completion observe should succeed");

        assert_eq!(failed.status, WorkerStatus::Failed);
        let error = failed.last_error.expect("provider error should exist");
        assert_eq!(error.kind, WorkerFailureKind::Provider);
        assert!(error.message.contains("provider degraded"));
        assert!(failed
            .events
            .iter()
            .any(|event| event.kind == WorkerEventKind::Failed));
    }

    #[test]
    fn emit_state_file_writes_worker_status_on_transition() {
        let cwd_path = std::env::temp_dir().join(format!(
            "claw-state-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&cwd_path).expect("test dir should create");
        let cwd = cwd_path.to_str().expect("test path should be utf8");
        let registry = WorkerRegistry::new();
        let worker = registry.create(cwd, &[], true);

        // After create the worker is Spawning — state file should exist
        let state_path = cwd_path.join(".claw").join("worker-state.json");
        assert!(
            state_path.exists(),
            "state file should exist after worker creation"
        );

        let raw = std::fs::read_to_string(&state_path).expect("state file should be readable");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("state file should be valid JSON");
        assert_eq!(
            value["status"].as_str(),
            Some("spawning"),
            "initial status should be spawning"
        );
        assert_eq!(value["is_ready"].as_bool(), Some(false));

        // Transition to ReadyForPrompt by observing trust-cleared text
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("observe ready should succeed");

        let raw = std::fs::read_to_string(&state_path)
            .expect("state file should be readable after observe");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("state file should be valid JSON after observe");
        assert_eq!(
            value["status"].as_str(),
            Some("ready_for_prompt"),
            "status should be ready_for_prompt after observe"
        );
        assert_eq!(
            value["is_ready"].as_bool(),
            Some(true),
            "is_ready should be true when ReadyForPrompt"
        );
    }

    #[test]
    fn observe_completion_accepts_normal_finish_with_tokens() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-g", &[], true);
        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        registry
            .send_prompt(&worker.worker_id, Some("Run tests"), None)
            .expect("prompt send should succeed");

        let finished = registry
            .observe_completion(&worker.worker_id, "stop", 150)
            .expect("completion observe should succeed");

        assert_eq!(finished.status, WorkerStatus::Finished);
        assert!(finished.last_error.is_none());
        assert!(finished
            .events
            .iter()
            .any(|event| event.kind == WorkerEventKind::Finished));
    }

    #[test]
    fn startup_timeout_emits_evidence_bundle_with_classification() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-timeout", &[], true);

        // Simulate startup timeout with transport dead
        let timed_out = registry
            .observe_startup_timeout(&worker.worker_id, "cargo test", false, true)
            .expect("startup timeout observe should succeed");

        assert_eq!(timed_out.status, WorkerStatus::Failed);
        let error = timed_out
            .last_error
            .expect("startup timeout error should exist");
        assert_eq!(error.kind, WorkerFailureKind::StartupNoEvidence);
        // Check for "TransportDead" (the Debug representation of the enum variant)
        assert!(
            error.message.contains("TransportDead"),
            "expected TransportDead in: {}",
            error.message
        );

        let event = timed_out
            .events
            .iter()
            .find(|e| e.kind == WorkerEventKind::StartupNoEvidence)
            .expect("startup no evidence event should exist");

        match event.payload.as_ref() {
            Some(WorkerEventPayload::StartupNoEvidence {
                evidence,
                classification,
            }) => {
                assert_eq!(
                    evidence.last_lifecycle_state,
                    WorkerStatus::Spawning,
                    "last state should be spawning"
                );
                assert_eq!(evidence.pane_command, "cargo test");
                assert!(evidence.command_started_at <= evidence.pane_observed_at);
                assert!(evidence.last_lifecycle_at <= evidence.pane_observed_at);
                assert!(!evidence.transport_healthy);
                assert!(!evidence.transport_health.healthy);
                assert!(evidence
                    .transport_health
                    .summary
                    .contains("transport_unhealthy"));
                assert!(evidence.mcp_healthy);
                assert!(evidence.mcp_health.healthy);
                assert_eq!(*classification, StartupFailureClassification::TransportDead);
            }
            _ => panic!(
                "expected StartupNoEvidence payload, got {:?}",
                event.payload
            ),
        }
    }

    #[test]
    fn startup_timeout_classifies_trust_required_when_prompt_blocked() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-trust", &[], false);

        // Simulate trust prompt detected but not resolved
        registry
            .observe(
                &worker.worker_id,
                "Do you trust the files in this folder?\n1. Yes, proceed\n2. No",
            )
            .expect("trust observe should succeed");

        // Now simulate startup timeout
        let timed_out = registry
            .observe_startup_timeout(&worker.worker_id, "claw prompt", true, true)
            .expect("startup timeout observe should succeed");

        let event = timed_out
            .events
            .iter()
            .find(|e| e.kind == WorkerEventKind::StartupNoEvidence)
            .expect("startup no evidence event should exist");

        match event.payload.as_ref() {
            Some(WorkerEventPayload::StartupNoEvidence { classification, .. }) => {
                assert_eq!(
                    *classification,
                    StartupFailureClassification::TrustRequired,
                    "should classify as trust_required when trust prompt detected"
                );
            }
            _ => panic!("expected StartupNoEvidence payload"),
        }
    }

    #[test]
    fn startup_timeout_classifies_prompt_acceptance_timeout() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-accept", &[], true);

        // Get worker to ReadyForPrompt
        registry
            .observe(&worker.worker_id, "Ready for your input\n>")
            .expect("ready observe should succeed");

        // Send prompt but don't get acceptance
        registry
            .send_prompt(&worker.worker_id, Some("Run tests"), None)
            .expect("prompt send should succeed");

        // Simulate startup timeout while prompt is still in flight
        let timed_out = registry
            .observe_startup_timeout(&worker.worker_id, "claw prompt", true, true)
            .expect("startup timeout observe should succeed");

        let event = timed_out
            .events
            .iter()
            .find(|e| e.kind == WorkerEventKind::StartupNoEvidence)
            .expect("startup no evidence event should exist");

        match event.payload.as_ref() {
            Some(WorkerEventPayload::StartupNoEvidence {
                evidence,
                classification,
            }) => {
                assert!(
                    evidence.prompt_sent_at.is_some(),
                    "should have prompt_sent_at"
                );
                assert!(!evidence.prompt_acceptance_state, "prompt not yet accepted");
                assert_eq!(
                    *classification,
                    StartupFailureClassification::PromptAcceptanceTimeout
                );
            }
            _ => panic!("expected StartupNoEvidence payload"),
        }
    }

    #[test]
    fn startup_timeout_preserves_original_prompt_sent_timestamp() {
        let registry = WorkerRegistry::new();
        let worker = registry.create("/tmp/repo-prompt-timestamp", &[], true);

        registry
            .observe(&worker.worker_id, "Ready for input\n>")
            .expect("ready observe should succeed");
        let prompted = registry
            .send_prompt(
                &worker.worker_id,
                Some("Run timestamp-sensitive work"),
                None,
            )
            .expect("prompt send should succeed");
        let sent_at = prompted
            .prompt_sent_at
            .expect("prompt send should record a prompt timestamp");

        let timed_out = registry
            .observe_startup_timeout(&worker.worker_id, "claw worker", true, true)
            .expect("startup timeout observe should succeed");

        let event = timed_out
            .events
            .iter()
            .find(|e| e.kind == WorkerEventKind::StartupNoEvidence)
            .expect("startup no evidence event should exist");

        match event.payload.as_ref() {
            Some(WorkerEventPayload::StartupNoEvidence { evidence, .. }) => {
                assert_eq!(evidence.prompt_sent_at, Some(sent_at));
                assert!(evidence.last_lifecycle_at <= evidence.pane_observed_at);
                assert!(evidence.command_started_at <= sent_at);
            }
            _ => panic!("expected StartupNoEvidence payload"),
        }
    }

    #[test]
    fn startup_evidence_bundle_serializes_correctly() {
        let bundle = StartupEvidenceBundle {
            last_lifecycle_state: WorkerStatus::Running,
            last_lifecycle_at: 1_234_567_889,
            pane_command: "test command".to_string(),
            pane_observed_at: 1_234_567_891,
            command_started_at: 1_234_567_800,
            prompt_sent_at: Some(1_234_567_890),
            prompt_acceptance_state: false,
            trust_prompt_detected: true,
            tool_permission_prompt_detected: false,
            tool_permission_prompt_age_seconds: None,
            tool_permission_allow_scope: None,
            transport_healthy: true,
            transport_health: StartupHealthSummary::observed("transport", true),
            mcp_healthy: false,
            mcp_health: StartupHealthSummary::observed("mcp", false),
            elapsed_seconds: 60,
        };

        let json = serde_json::to_string(&bundle).expect("should serialize");
        assert!(json.contains("\"last_lifecycle_state\""));
        assert!(json.contains("\"pane_command\""));
        assert!(json.contains("\"prompt_sent_at\":1234567890"));
        assert!(json.contains("\"trust_prompt_detected\":true"));
        assert!(json.contains("\"last_lifecycle_at\":1234567889"));
        assert!(json.contains("\"pane_observed_at\":1234567891"));
        assert!(json.contains("\"command_started_at\":1234567800"));
        assert!(json.contains("\"transport_healthy\":true"));
        assert!(json.contains("\"transport_health\""));
        assert!(json.contains("\"mcp_healthy\":false"));
        assert!(json.contains("\"mcp_health\""));

        let deserialized: StartupEvidenceBundle =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized.last_lifecycle_state, WorkerStatus::Running);
        assert_eq!(deserialized.prompt_sent_at, Some(1_234_567_890));
    }

    #[test]
    fn classify_startup_failure_detects_transport_dead() {
        let evidence = StartupEvidenceBundle {
            last_lifecycle_state: WorkerStatus::Spawning,
            last_lifecycle_at: 10,
            pane_command: "test".to_string(),
            pane_observed_at: 40,
            command_started_at: 1,
            prompt_sent_at: None,
            prompt_acceptance_state: false,
            trust_prompt_detected: false,
            tool_permission_prompt_detected: false,
            tool_permission_prompt_age_seconds: None,
            tool_permission_allow_scope: None,
            transport_healthy: false,
            transport_health: StartupHealthSummary::observed("transport", false),
            mcp_healthy: true,
            mcp_health: StartupHealthSummary::observed("mcp", true),
            elapsed_seconds: 30,
        };

        let classification = classify_startup_failure(&evidence);
        assert_eq!(classification, StartupFailureClassification::TransportDead);
    }

    #[test]
    fn classify_startup_failure_defaults_to_unknown() {
        let evidence = StartupEvidenceBundle {
            last_lifecycle_state: WorkerStatus::Spawning,
            last_lifecycle_at: 10,
            pane_command: "test".to_string(),
            pane_observed_at: 40,
            command_started_at: 1,
            prompt_sent_at: None,
            prompt_acceptance_state: false,
            trust_prompt_detected: false,
            tool_permission_prompt_detected: false,
            tool_permission_prompt_age_seconds: None,
            tool_permission_allow_scope: None,
            transport_healthy: true,
            transport_health: StartupHealthSummary::observed("transport", true),
            mcp_healthy: true,
            mcp_health: StartupHealthSummary::observed("mcp", true),
            elapsed_seconds: 10,
        };

        let classification = classify_startup_failure(&evidence);
        assert_eq!(classification, StartupFailureClassification::Unknown);
    }

    #[test]
    fn classify_startup_failure_detects_prompt_misdelivery_after_timeout() {
        let evidence = StartupEvidenceBundle {
            last_lifecycle_state: WorkerStatus::ReadyForPrompt,
            last_lifecycle_at: 10,
            pane_command: "test".to_string(),
            pane_observed_at: 45,
            command_started_at: 1,
            prompt_sent_at: Some(10),
            prompt_acceptance_state: false,
            trust_prompt_detected: false,
            tool_permission_prompt_detected: false,
            tool_permission_prompt_age_seconds: None,
            tool_permission_allow_scope: None,
            transport_healthy: true,
            transport_health: StartupHealthSummary::observed("transport", true),
            mcp_healthy: true,
            mcp_health: StartupHealthSummary::observed("mcp", true),
            elapsed_seconds: 31,
        };

        let classification = classify_startup_failure(&evidence);
        assert_eq!(
            classification,
            StartupFailureClassification::PromptMisdelivery
        );
    }

    #[test]
    fn classify_startup_failure_detects_worker_crashed() {
        // Worker crashed scenario: transport healthy but MCP unhealthy
        // Don't have prompt in flight (no prompt_sent_at) to avoid matching PromptAcceptanceTimeout
        let evidence = StartupEvidenceBundle {
            last_lifecycle_state: WorkerStatus::Spawning,
            last_lifecycle_at: 10,
            pane_command: "test".to_string(),
            pane_observed_at: 40,
            command_started_at: 1,
            prompt_sent_at: None, // No prompt sent yet
            prompt_acceptance_state: false,
            trust_prompt_detected: false,
            tool_permission_prompt_detected: false,
            tool_permission_prompt_age_seconds: None,
            tool_permission_allow_scope: None,
            transport_healthy: true,
            transport_health: StartupHealthSummary::observed("transport", true),
            mcp_healthy: false,
            mcp_health: StartupHealthSummary::observed("mcp", false), // MCP unhealthy but transport healthy suggests crash
            elapsed_seconds: 45,
        };

        let classification = classify_startup_failure(&evidence);
        assert_eq!(classification, StartupFailureClassification::WorkerCrashed);
    }
}
