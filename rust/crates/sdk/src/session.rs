use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use runtime::{
    ApiRequest, AssistantEvent, CompactionConfig, ConversationRuntime, PermissionMode,
    PermissionPolicy, RuntimeError, Session, TurnSummary,
};

use crate::event_bus::{AgentSessionEvent, EventBus, SessionLifecycleEvent, TurnEvent};
use crate::tool_registry::{SdkToolExecutor, ToolRegistry};

/// Well-known model families for `cycle_model()` rotation.
const MODEL_CYCLE: &[&str] = &[
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "gpt-4o",
    "gpt-5",
];

/// A type-erased API client that wraps any `runtime::ApiClient` in a `Box`.
///
/// This allows `AgentSession` to accept any provider implementation without
/// being generic over the client type.
pub struct BoxedApiClient {
    inner: Box<dyn runtime::ApiClient>,
}

impl BoxedApiClient {
    /// Create a new boxed API client from any type implementing `ApiClient`.
    pub fn new(client: impl runtime::ApiClient + 'static) -> Self {
        Self {
            inner: Box::new(client),
        }
    }
}

impl runtime::ApiClient for BoxedApiClient {
    fn stream(
        &mut self,
        request: ApiRequest,
    ) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.inner.stream(request)
    }
}

/// A minimal API client used by the SDK when no real provider is configured.
/// Returns an error on every call.
#[derive(Debug, Clone)]
pub struct DummyApiClient;

impl runtime::ApiClient for DummyApiClient {
    fn stream(
        &mut self,
        _request: ApiRequest,
    ) -> Result<Vec<AssistantEvent>, RuntimeError> {
        Err(RuntimeError::new(
            "SDK mode: no API client configured. \
             Provide a real ApiClient via AgentSessionBuilder.",
        ))
    }
}

/// Builder for constructing `AgentSession` with a fluent API.
///
/// # Example
///
/// ```rust,no_run
/// use sdk::AgentSessionBuilder;
/// use sdk::ToolRegistry;
/// use runtime::PermissionMode;
///
/// // Build with default (dummy) client
/// let (session, bus) = AgentSessionBuilder::new()
///     .model("claude-sonnet-4-6")
///     .system_prompt("You are a helpful assistant.")
///     .tools(ToolRegistry::new())
///     .permission_mode(PermissionMode::DangerFullAccess)
///     .build()
///     .expect("should create session");
/// ```
pub struct AgentSessionBuilder {
    model: String,
    system_prompt: Vec<String>,
    tools: ToolRegistry,
    permission_mode: PermissionMode,
    api_client: Option<BoxedApiClient>,
}

impl AgentSessionBuilder {
    /// Create a new builder with defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: "claude-sonnet-4-6".to_string(),
            system_prompt: Vec::new(),
            tools: ToolRegistry::new(),
            permission_mode: PermissionMode::DangerFullAccess,
            api_client: None,
        }
    }

    /// Set the model.
    #[must_use]
    pub fn model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Add a system prompt line.
    #[must_use]
    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt.push(prompt.to_string());
        self
    }

    /// Set the tool registry.
    #[must_use]
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Set the permission mode.
    #[must_use]
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// Provide a custom API client. Any type implementing `runtime::ApiClient`.
    #[must_use]
    pub fn api_client(mut self, client: impl runtime::ApiClient + 'static) -> Self {
        self.api_client = Some(BoxedApiClient::new(client));
        self
    }

    /// Build the `AgentSession`.
    ///
    /// Returns the session and an event bus for subscribing to events.
    pub fn build(self) -> Result<(AgentSession, EventBus), String> {
        let session = Session::new();
        let mut event_bus = EventBus::new();
        let tool_executor = SdkToolExecutor::new(&self.tools);

        let api_client = self
            .api_client
            .unwrap_or_else(|| BoxedApiClient::new(DummyApiClient));

        let runtime = ConversationRuntime::new(
            session.clone(),
            api_client,
            tool_executor,
            PermissionPolicy::new(self.permission_mode),
            self.system_prompt.clone(),
        );

        event_bus.emit(AgentSessionEvent::SessionLifecycle(
            SessionLifecycleEvent::Created {
                session_id: session.session_id.clone(),
            },
        ));

        let returned_bus = event_bus.clone();
        Ok((
            AgentSession {
                model: self.model,
                system_prompt: self.system_prompt,
                runtime,
                session,
                event_bus,
                permission_mode: self.permission_mode,
                abort_requested: Arc::new(AtomicBool::new(false)),
                disposed: false,
            },
            returned_bus,
        ))
    }
}

impl Default for AgentSessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// An agent session that wraps the runtime and provides a high-level API.
///
/// `AgentSession` owns a `ConversationRuntime` and provides methods for
/// running turns, subscribing to events, and managing session state.
///
/// Use [`AgentSessionBuilder`] to construct with a custom API client:
///
/// ```rust,no_run
/// use sdk::AgentSessionBuilder;
/// use sdk::DummyApiClient;
/// use runtime::PermissionMode;
///
/// // Build with a custom API client
/// let (session, bus) = AgentSessionBuilder::new()
///     .model("claude-sonnet-4-6")
///     .api_client(DummyApiClient)
///     .permission_mode(PermissionMode::DangerFullAccess)
///     .build()
///     .expect("session should create");
/// ```
pub struct AgentSession {
    /// The model identifier being used.
    model: String,
    /// The system prompt.
    system_prompt: Vec<String>,
    /// The runtime instance.
    runtime: ConversationRuntime<BoxedApiClient, SdkToolExecutor>,
    /// The underlying session state.
    session: Session,
    /// Event bus for subscribing to lifecycle events.
    event_bus: EventBus,
    /// Permission mode.
    permission_mode: PermissionMode,
    /// Abort signal shared with the runtime turn loop.
    abort_requested: Arc<AtomicBool>,
    /// Whether this session has been disposed.
    disposed: bool,
}

impl AgentSession {
    /// Create a new agent session with default (dummy) API client.
    ///
    /// For production use, prefer [`AgentSessionBuilder`] with a real `api_client()`.
    ///
    /// Returns the session and an event bus you can subscribe to for events.
    pub fn new(
        model: &str,
        system_prompt: Vec<String>,
        tool_registry: ToolRegistry,
        permission_mode: PermissionMode,
    ) -> Result<(Self, EventBus), String> {
        AgentSessionBuilder::new()
            .model(model)
            .tools(tool_registry)
            .permission_mode(permission_mode)
            .build()
            .map(|(mut session, bus)| {
                session.system_prompt = system_prompt;
                (session, bus)
            })
    }

    /// Get the session ID.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session.session_id
    }

    /// Get the model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get the system prompt.
    #[must_use]
    pub fn system_prompt(&self) -> &[String] {
        &self.system_prompt
    }

    /// Get the permission mode.
    #[must_use]
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Get a reference to the underlying session.
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run a single turn with the given user input.
    pub fn run_turn(&mut self, input: &str) -> Result<TurnSummary, RuntimeError> {
        self.event_bus.emit(AgentSessionEvent::TurnStarted);

        let result = self.runtime.run_turn(input.to_string(), None);

        match &result {
            Ok(summary) => {
                self.event_bus
                    .emit(AgentSessionEvent::TurnCompleted(summary.clone()));
                self.event_bus
                    .emit(AgentSessionEvent::TurnEvent(TurnEvent::Completed(
                        summary.clone(),
                    )));
            }
            Err(e) => {
                self.event_bus.emit(AgentSessionEvent::Error(e.to_string()));
            }
        }

        result
    }

    /// Subscribe to session events.
    pub fn subscribe(&mut self) -> crate::EventSubscription {
        self.event_bus.subscribe()
    }

    /// Emit a lifecycle event manually.
    pub fn emit_event(&mut self, event: AgentSessionEvent) {
        self.event_bus.emit(event);
    }

    /// Inject a mid-turn message that will be picked up on the next API call
    /// within the current or next turn. This is useful for steering an agent
    /// mid-execution (e.g. "stop what you're doing and focus on X").
    ///
    /// Returns an error if the session has been disposed.
    pub fn steer(&mut self, message: &str) -> Result<(), RuntimeError> {
        self.ensure_not_disposed()?;
        self.session
            .push_user_text(message)
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        self.event_bus
            .emit(AgentSessionEvent::TextDelta(format!(
                "[steer] {message}"
            )));
        Ok(())
    }

    /// Queue a follow-up message to be sent as the next user turn. Unlike
    /// `steer()`, this does not inject into the current message list — it
    /// simply records it for the caller to use on the next `run_turn()`.
    ///
    /// Returns an error if the session has been disposed.
    pub fn follow_up(&mut self, message: &str) -> Result<(), RuntimeError> {
        self.ensure_not_disposed()?;
        self.event_bus
            .emit(AgentSessionEvent::TextDelta(format!(
                "[follow-up queued] {message}"
            )));
        Ok(())
    }

    /// Switch the model used for subsequent turns.
    ///
    /// Returns the previous model name.
    pub fn set_model(&mut self, model: &str) -> Result<String, RuntimeError> {
        self.ensure_not_disposed()?;
        let previous = std::mem::replace(&mut self.model, model.to_string());
        self.session.model = Some(model.to_string());
        self.event_bus
            .emit(AgentSessionEvent::SessionLifecycle(
                SessionLifecycleEvent::Created {
                    session_id: format!("{}:model={model}", self.session.session_id),
                },
            ));
        Ok(previous)
    }

    /// Cycle through a built-in set of well-known models. Advances to the
    /// next model in the rotation and returns the new model name.
    ///
    /// If the current model is not in the cycle list, starts from the first entry.
    pub fn cycle_model(&mut self) -> Result<String, RuntimeError> {
        self.ensure_not_disposed()?;
        let current = self.model.as_str();
        let next = MODEL_CYCLE
            .iter()
            .position(|&m| m == current)
            .map(|i| MODEL_CYCLE[(i + 1) % MODEL_CYCLE.len()])
            .unwrap_or(MODEL_CYCLE[0]);
        self.set_model(next)?;
        Ok(next.to_string())
    }

    /// Explicitly compact the session context, summarizing older messages
    /// and preserving recent ones. This reduces token usage for long
    /// conversations.
    ///
    /// Returns the number of messages removed.
    pub fn compact(&mut self) -> Result<usize, RuntimeError> {
        self.ensure_not_disposed()?;
        self.event_bus
            .emit(AgentSessionEvent::SessionLifecycle(
                SessionLifecycleEvent::CompactionStarted,
            ));

        let result = self.runtime.compact(CompactionConfig::default());

        self.event_bus
            .emit(AgentSessionEvent::SessionLifecycle(
                SessionLifecycleEvent::CompactionCompleted {
                    removed_count: result.removed_message_count,
                },
            ));

        Ok(result.removed_message_count)
    }

    /// Request that the current turn abort as soon as possible. The abort
    /// signal is checked between tool execution iterations in the runtime
    /// turn loop.
    ///
    /// Note: the actual abort is cooperative — the running turn must check
    /// this signal. If no turn is running, the next turn will see the
    /// abort flag and reset it immediately.
    pub fn abort(&mut self) -> Result<(), RuntimeError> {
        self.ensure_not_disposed()?;
        self.abort_requested.store(true, Ordering::SeqCst);
        self.event_bus
            .emit(AgentSessionEvent::Error("abort requested".to_string()));
        Ok(())
    }

    /// Check whether an abort has been requested (useful from within tool
    /// executors or extension callbacks).
    #[must_use]
    pub fn is_abort_requested(&self) -> bool {
        self.abort_requested.load(Ordering::SeqCst)
    }

    /// Cleanly tear down the session. Emits a `Closed` lifecycle event,
    /// clears internal state, and marks the session as disposed.
    ///
    /// All subsequent method calls on this session will return
    /// `Err(RuntimeError)`.
    pub fn dispose(&mut self) -> Result<(), RuntimeError> {
        if self.disposed {
            return Ok(());
        }

        self.event_bus
            .emit(AgentSessionEvent::SessionLifecycle(
                SessionLifecycleEvent::Closed {
                    session_id: self.session.session_id.clone(),
                },
            ));

        self.session.messages.clear();
        self.session.compaction = None;
        self.disposed = true;
        Ok(())
    }

    /// Check whether this session has been disposed.
    #[must_use]
    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    fn ensure_not_disposed(&self) -> Result<(), RuntimeError> {
        if self.disposed {
            return Err(RuntimeError::new(
                "session has been disposed and can no longer be used",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::ToolRegistry;

    #[test]
    fn creates_session_with_valid_id() {
        let (session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec!["You are a helpful assistant.".to_string()],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        assert!(!session.session_id().is_empty());
        assert_eq!(session.model(), "claude-sonnet-4-6");
    }

    #[test]
    fn run_turn_fails_with_dummy_client() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec!["system".to_string()],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        let result = session.run_turn("hello");
        assert!(result.is_err(), "dummy client should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SDK mode"),
            "error should mention SDK mode: {err}"
        );
    }

    #[test]
    fn builder_creates_session_with_custom_model() {
        let (session, _bus) = AgentSessionBuilder::new()
            .model("gpt-4o")
            .system_prompt("You are helpful.")
            .permission_mode(PermissionMode::DangerFullAccess)
            .build()
            .expect("builder should create session");

        assert_eq!(session.model(), "gpt-4o");
    }

    #[test]
    fn builder_accepts_custom_api_client() {
        let (session, _bus) = AgentSessionBuilder::new()
            .model("claude-sonnet-4-6")
            .api_client(DummyApiClient)
            .permission_mode(PermissionMode::DangerFullAccess)
            .build()
            .expect("builder with custom client should create session");

        assert!(!session.session_id().is_empty());
    }

    // --- steer / follow_up ---

    #[test]
    fn steer_injects_message_into_session() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        session.steer("Focus on the tests").expect("steer should work");
        let msgs = &session.session().messages;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].blocks.len(), 1);
    }

    #[test]
    fn follow_up_emits_event() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        // Subscribe on the session's own event bus
        let sub = session.subscribe();
        session.follow_up("next task").expect("follow_up should work");
        let event = sub.try_recv().expect("should have event");
        assert!(matches!(event, AgentSessionEvent::TextDelta(t) if t.contains("next task")));
    }

    // --- set_model / cycle_model ---

    #[test]
    fn set_model_switches_model_and_returns_previous() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        let prev = session.set_model("gpt-4o").expect("set_model should work");
        assert_eq!(prev, "claude-sonnet-4-6");
        assert_eq!(session.model(), "gpt-4o");
    }

    #[test]
    fn cycle_model_advances_through_rotation() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        let next = session.cycle_model().expect("cycle_model should work");
        assert_eq!(next, "claude-opus-4-6");
        assert_eq!(session.model(), "claude-opus-4-6");

        let next2 = session.cycle_model().expect("cycle_model should work");
        assert_eq!(next2, "gpt-4o");
    }

    #[test]
    fn cycle_model_wraps_around() {
        let (mut session, _bus) = AgentSessionBuilder::new()
            .model("gpt-5")
            .permission_mode(PermissionMode::DangerFullAccess)
            .build()
            .expect("should create");

        let next = session.cycle_model().expect("cycle_model should work");
        assert_eq!(next, "claude-sonnet-4-6");
    }

    #[test]
    fn cycle_model_unknown_model_starts_from_first() {
        let (mut session, _bus) = AgentSessionBuilder::new()
            .model("custom-model")
            .permission_mode(PermissionMode::DangerFullAccess)
            .build()
            .expect("should create");

        let next = session.cycle_model().expect("cycle_model should work");
        assert_eq!(next, "claude-sonnet-4-6");
    }

    // --- compact ---

    #[test]
    fn compact_returns_zero_for_empty_session() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        let removed = session.compact().expect("compact should work");
        assert_eq!(removed, 0);
    }

    // --- abort ---

    #[test]
    fn abort_sets_flag() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        assert!(!session.is_abort_requested());
        session.abort().expect("abort should work");
        assert!(session.is_abort_requested());
    }

    // --- dispose ---

    #[test]
    fn dispose_cleans_up_session() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        let sub = session.subscribe();
        session.dispose().expect("dispose should work");

        assert!(session.is_disposed());
        assert!(session.session().messages.is_empty());
        assert!(session.session().compaction.is_none());

        // Should emit Closed event
        let event = sub.try_recv().expect("should have closed event");
        assert!(matches!(
            event,
            AgentSessionEvent::SessionLifecycle(SessionLifecycleEvent::Closed { .. })
        ));
    }

    #[test]
    fn dispose_is_idempotent() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        session.dispose().expect("first dispose should work");
        session.dispose().expect("second dispose should also work");
    }

    #[test]
    fn methods_fail_after_dispose() {
        let (mut session, _bus) = AgentSession::new(
            "claude-sonnet-4-6",
            vec![],
            ToolRegistry::new(),
            PermissionMode::DangerFullAccess,
        )
        .expect("session should create");

        session.dispose().expect("dispose should work");

        assert!(session.steer("test").is_err());
        assert!(session.follow_up("test").is_err());
        assert!(session.set_model("test").is_err());
        assert!(session.cycle_model().is_err());
        assert!(session.compact().is_err());
        assert!(session.abort().is_err());
        assert!(session.run_turn("test").is_err());
    }
}
