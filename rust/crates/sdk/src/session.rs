use runtime::{
    ApiRequest, AssistantEvent, ConversationRuntime, PermissionMode, PermissionPolicy,
    RuntimeError, Session, TurnSummary,
};

use crate::event_bus::{AgentSessionEvent, EventBus, SessionLifecycleEvent, TurnEvent};
use crate::tool_registry::{SdkToolExecutor, ToolRegistry};

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
}
