//! # Claw SDK
//!
//! Programmatic API for embedding Claw's agent capabilities in Rust applications.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use sdk::AgentSession;
//! use runtime::PermissionMode;
//!
//! // Create a session (event bus is created internally)
//! let (mut session, mut event_bus) = AgentSession::new(
//!     "claude-sonnet-4-6",
//!     vec!["You are a helpful assistant.".to_string()],
//!     sdk::ToolRegistry::new(),
//!     PermissionMode::DangerFullAccess,
//! )
//! .expect("session should create");
//!
//! // Subscribe to events via the returned event bus
//! let _sub = event_bus.subscribe();
//!
//! // Run a turn (will fail with dummy client; use real ApiClient in production)
//! let result = session.run_turn("Hello, what files are here?");
//! assert!(result.is_err()); // Dummy client always fails

mod agent_context;
mod event_bus;
mod extension;
mod resource_loader;
#[cfg(feature = "rpc")]
mod rpc;
mod session;
mod session_manager;
mod session_tree;
mod tool_registry;

pub use agent_context::{AgentContext, AgentTask, TaskRegistry};
pub use event_bus::{
    AgentSessionEvent, EventBus, EventSubscription, SessionLifecycleEvent, ToolExecutionEvent,
    TurnEvent,
};
pub use extension::{Extension, ExtensionRegistry, SimpleExtension};
pub use resource_loader::{DefaultResourceLoader, ResourceLoader};
pub use session::{AgentSession, AgentSessionBuilder, BoxedApiClient, DummyApiClient};
pub use session_manager::{SessionManager, SessionManagerConfig};
pub use session_tree::{SessionTree, SessionTreeNode};
pub use tool_registry::{create_builtin_tools, SdkToolExecutor, ToolRegistry};

#[cfg(feature = "rpc")]
pub use rpc::run_rpc_server;

// Re-export key runtime types for convenience
pub use runtime::{
    ConversationRuntime, PermissionMode, RuntimeError, Session, StaticToolExecutor, ToolError,
    ToolExecutor, TurnSummary,
};
