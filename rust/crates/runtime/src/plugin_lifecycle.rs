#![allow(clippy::redundant_closure_for_method_calls)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::RuntimePluginConfig;
use crate::mcp_tool_bridge::{McpResourceInfo, McpToolInfo};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub type ToolInfo = McpToolInfo;
pub type ResourceInfo = McpResourceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Healthy,
    Degraded,
    Failed,
}

impl std::fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHealth {
    pub server_name: String,
    pub status: ServerStatus,
    pub capabilities: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PluginState {
    Unconfigured,
    Validated,
    Starting,
    Healthy,
    Degraded {
        healthy_servers: Vec<String>,
        failed_servers: Vec<ServerHealth>,
    },
    Failed {
        reason: String,
    },
    ShuttingDown,
    Stopped,
}

impl PluginState {
    #[must_use]
    pub fn startup_event(&self) -> Option<PluginLifecycleEvent> {
        match self {
            Self::Healthy => Some(PluginLifecycleEvent::StartupHealthy),
            Self::Degraded { .. } => Some(PluginLifecycleEvent::StartupDegraded),
            Self::Failed { .. } => Some(PluginLifecycleEvent::StartupFailed),
            Self::Unconfigured
            | Self::Validated
            | Self::Starting
            | Self::ShuttingDown
            | Self::Stopped => None,
        }
    }

    #[must_use]
    pub fn is_startup_terminal(&self) -> bool {
        self.startup_event().is_some()
    }

    #[must_use]
    pub fn from_servers(servers: &[ServerHealth]) -> Self {
        if servers.is_empty() {
            return Self::Failed {
                reason: "no servers available".to_string(),
            };
        }

        let healthy_servers = servers
            .iter()
            .filter(|server| server.status != ServerStatus::Failed)
            .map(|server| server.server_name.clone())
            .collect::<Vec<_>>();
        let failed_servers = servers
            .iter()
            .filter(|server| server.status == ServerStatus::Failed)
            .cloned()
            .collect::<Vec<_>>();
        let has_degraded_server = servers
            .iter()
            .any(|server| server.status == ServerStatus::Degraded);

        if failed_servers.is_empty() && !has_degraded_server {
            Self::Healthy
        } else if healthy_servers.is_empty() {
            Self::Failed {
                reason: format!("all {} servers failed", failed_servers.len()),
            }
        } else {
            Self::Degraded {
                healthy_servers,
                failed_servers,
            }
        }
    }
}

impl std::fmt::Display for PluginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfigured => write!(f, "unconfigured"),
            Self::Validated => write!(f, "validated"),
            Self::Starting => write!(f, "starting"),
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded { .. } => write!(f, "degraded"),
            Self::Failed { .. } => write!(f, "failed"),
            Self::ShuttingDown => write!(f, "shutting_down"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHealthcheck {
    pub plugin_name: String,
    pub state: PluginState,
    pub servers: Vec<ServerHealth>,
    pub last_check: u64,
}

impl PluginHealthcheck {
    #[must_use]
    pub fn startup_event(&self) -> Option<PluginLifecycleEvent> {
        self.state.startup_event()
    }

    #[must_use]
    pub fn new(plugin_name: impl Into<String>, servers: Vec<ServerHealth>) -> Self {
        let state = PluginState::from_servers(&servers);
        Self {
            plugin_name: plugin_name.into(),
            state,
            servers,
            last_check: now_secs(),
        }
    }

    #[must_use]
    pub fn degraded_mode(&self, discovery: &DiscoveryResult) -> Option<DegradedMode> {
        match &self.state {
            PluginState::Degraded {
                healthy_servers,
                failed_servers,
            } => Some(DegradedMode {
                available_tools: discovery
                    .tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect(),
                unavailable_tools: failed_servers
                    .iter()
                    .flat_map(|server| server.capabilities.iter().cloned())
                    .collect(),
                reason: format!(
                    "{} servers healthy, {} servers failed",
                    healthy_servers.len(),
                    failed_servers.len()
                ),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub tools: Vec<ToolInfo>,
    pub resources: Vec<ResourceInfo>,
    pub partial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradedMode {
    pub available_tools: Vec<String>,
    pub unavailable_tools: Vec<String>,
    pub reason: String,
}

impl DegradedMode {
    #[must_use]
    pub fn new(
        available_tools: Vec<String>,
        unavailable_tools: Vec<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            available_tools,
            unavailable_tools,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginLifecycleEvent {
    ConfigValidated,
    StartupHealthy,
    StartupDegraded,
    StartupFailed,
    Shutdown,
}

impl std::fmt::Display for PluginLifecycleEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigValidated => write!(f, "config_validated"),
            Self::StartupHealthy => write!(f, "startup_healthy"),
            Self::StartupDegraded => write!(f, "startup_degraded"),
            Self::StartupFailed => write!(f, "startup_failed"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

pub trait PluginLifecycle {
    fn validate_config(&self, config: &RuntimePluginConfig) -> Result<(), String>;
    fn healthcheck(&self) -> PluginHealthcheck;
    fn discover(&self) -> DiscoveryResult;
    fn shutdown(&mut self) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct MockPluginLifecycle {
        plugin_name: String,
        valid_config: bool,
        healthcheck: PluginHealthcheck,
        discovery: DiscoveryResult,
        shutdown_error: Option<String>,
        shutdown_called: bool,
    }

    impl MockPluginLifecycle {
        fn new(
            plugin_name: &str,
            valid_config: bool,
            servers: Vec<ServerHealth>,
            discovery: DiscoveryResult,
            shutdown_error: Option<String>,
        ) -> Self {
            Self {
                plugin_name: plugin_name.to_string(),
                valid_config,
                healthcheck: PluginHealthcheck::new(plugin_name, servers),
                discovery,
                shutdown_error,
                shutdown_called: false,
            }
        }
    }

    impl PluginLifecycle for MockPluginLifecycle {
        fn validate_config(&self, _config: &RuntimePluginConfig) -> Result<(), String> {
            if self.valid_config {
                Ok(())
            } else {
                Err(format!(
                    "plugin `{}` failed configuration validation",
                    self.plugin_name
                ))
            }
        }

        fn healthcheck(&self) -> PluginHealthcheck {
            if self.shutdown_called {
                PluginHealthcheck {
                    plugin_name: self.plugin_name.clone(),
                    state: PluginState::Stopped,
                    servers: self.healthcheck.servers.clone(),
                    last_check: now_secs(),
                }
            } else {
                self.healthcheck.clone()
            }
        }

        fn discover(&self) -> DiscoveryResult {
            self.discovery.clone()
        }

        fn shutdown(&mut self) -> Result<(), String> {
            if let Some(error) = &self.shutdown_error {
                return Err(error.clone());
            }

            self.shutdown_called = true;
            Ok(())
        }
    }

    fn healthy_server(name: &str, capabilities: &[&str]) -> ServerHealth {
        ServerHealth {
            server_name: name.to_string(),
            status: ServerStatus::Healthy,
            capabilities: capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect(),
            last_error: None,
        }
    }

    fn failed_server(name: &str, capabilities: &[&str], error: &str) -> ServerHealth {
        ServerHealth {
            server_name: name.to_string(),
            status: ServerStatus::Failed,
            capabilities: capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect(),
            last_error: Some(error.to_string()),
        }
    }

    fn degraded_server(name: &str, capabilities: &[&str], error: &str) -> ServerHealth {
        ServerHealth {
            server_name: name.to_string(),
            status: ServerStatus::Degraded,
            capabilities: capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect(),
            last_error: Some(error.to_string()),
        }
    }

    fn tool(name: &str) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: Some(format!("{name} tool")),
            input_schema: None,
        }
    }

    fn resource(name: &str, uri: &str) -> ResourceInfo {
        ResourceInfo {
            uri: uri.to_string(),
            name: name.to_string(),
            description: Some(format!("{name} resource")),
            mime_type: Some("application/json".to_string()),
        }
    }

    #[test]
    fn startup_event_maps_terminal_health_states() {
        // given
        let healthy =
            PluginHealthcheck::new("healthy-plugin", vec![healthy_server("alpha", &["search"])]);
        let degraded = PluginHealthcheck::new(
            "degraded-plugin",
            vec![
                healthy_server("alpha", &["search"]),
                failed_server("beta", &["write"], "connection refused"),
            ],
        );
        let failed = PluginHealthcheck::new(
            "failed-plugin",
            vec![failed_server("beta", &["write"], "connection refused")],
        );

        // then
        assert_eq!(
            healthy.startup_event(),
            Some(PluginLifecycleEvent::StartupHealthy)
        );
        assert_eq!(
            degraded.startup_event(),
            Some(PluginLifecycleEvent::StartupDegraded)
        );
        assert_eq!(
            failed.startup_event(),
            Some(PluginLifecycleEvent::StartupFailed)
        );
        assert!(healthy.state.is_startup_terminal());
        assert_eq!(PluginState::Starting.startup_event(), None);
        assert!(!PluginState::Starting.is_startup_terminal());
    }

    #[test]
    fn full_lifecycle_happy_path() {
        // given
        let mut lifecycle = MockPluginLifecycle::new(
            "healthy-plugin",
            true,
            vec![
                healthy_server("alpha", &["search", "read"]),
                healthy_server("beta", &["write"]),
            ],
            DiscoveryResult {
                tools: vec![tool("search"), tool("read"), tool("write")],
                resources: vec![resource("docs", "file:///docs")],
                partial: false,
            },
            None,
        );
        let config = RuntimePluginConfig::default();

        // when
        let validation = lifecycle.validate_config(&config);
        let healthcheck = lifecycle.healthcheck();
        let discovery = lifecycle.discover();
        let shutdown = lifecycle.shutdown();
        let post_shutdown = lifecycle.healthcheck();

        // then
        assert_eq!(validation, Ok(()));
        assert_eq!(healthcheck.state, PluginState::Healthy);
        assert_eq!(healthcheck.plugin_name, "healthy-plugin");
        assert_eq!(discovery.tools.len(), 3);
        assert_eq!(discovery.resources.len(), 1);
        assert!(!discovery.partial);
        assert_eq!(shutdown, Ok(()));
        assert_eq!(post_shutdown.state, PluginState::Stopped);
    }

    #[test]
    fn degraded_startup_when_one_of_three_servers_fails() {
        // given
        let lifecycle = MockPluginLifecycle::new(
            "degraded-plugin",
            true,
            vec![
                healthy_server("alpha", &["search"]),
                failed_server("beta", &["write"], "connection refused"),
                healthy_server("gamma", &["read"]),
            ],
            DiscoveryResult {
                tools: vec![tool("search"), tool("read")],
                resources: vec![resource("alpha-docs", "file:///alpha")],
                partial: true,
            },
            None,
        );

        // when
        let healthcheck = lifecycle.healthcheck();
        let discovery = lifecycle.discover();
        let degraded_mode = healthcheck
            .degraded_mode(&discovery)
            .expect("degraded startup should expose degraded mode");

        // then
        match healthcheck.state {
            PluginState::Degraded {
                healthy_servers,
                failed_servers,
            } => {
                assert_eq!(
                    healthy_servers,
                    vec!["alpha".to_string(), "gamma".to_string()]
                );
                assert_eq!(failed_servers.len(), 1);
                assert_eq!(failed_servers[0].server_name, "beta");
                assert_eq!(
                    failed_servers[0].last_error.as_deref(),
                    Some("connection refused")
                );
            }
            other => panic!("expected degraded state, got {other:?}"),
        }
        assert!(discovery.partial);
        assert_eq!(
            degraded_mode.available_tools,
            vec!["search".to_string(), "read".to_string()]
        );
        assert_eq!(degraded_mode.unavailable_tools, vec!["write".to_string()]);
        assert_eq!(degraded_mode.reason, "2 servers healthy, 1 servers failed");
    }

    #[test]
    fn degraded_server_status_keeps_server_usable() {
        // given
        let lifecycle = MockPluginLifecycle::new(
            "soft-degraded-plugin",
            true,
            vec![
                healthy_server("alpha", &["search"]),
                degraded_server("beta", &["write"], "high latency"),
            ],
            DiscoveryResult {
                tools: vec![tool("search"), tool("write")],
                resources: Vec::new(),
                partial: true,
            },
            None,
        );

        // when
        let healthcheck = lifecycle.healthcheck();

        // then
        match healthcheck.state {
            PluginState::Degraded {
                healthy_servers,
                failed_servers,
            } => {
                assert_eq!(
                    healthy_servers,
                    vec!["alpha".to_string(), "beta".to_string()]
                );
                assert!(failed_servers.is_empty());
            }
            other => panic!("expected degraded state, got {other:?}"),
        }
    }

    #[test]
    fn complete_failure_when_all_servers_fail() {
        // given
        let lifecycle = MockPluginLifecycle::new(
            "failed-plugin",
            true,
            vec![
                failed_server("alpha", &["search"], "timeout"),
                failed_server("beta", &["read"], "handshake failed"),
            ],
            DiscoveryResult {
                tools: Vec::new(),
                resources: Vec::new(),
                partial: false,
            },
            None,
        );

        // when
        let healthcheck = lifecycle.healthcheck();
        let discovery = lifecycle.discover();

        // then
        match &healthcheck.state {
            PluginState::Failed { reason } => {
                assert_eq!(reason, "all 2 servers failed");
            }
            other => panic!("expected failed state, got {other:?}"),
        }
        assert!(!discovery.partial);
        assert!(discovery.tools.is_empty());
        assert!(discovery.resources.is_empty());
        assert!(healthcheck.degraded_mode(&discovery).is_none());
    }

    #[test]
    fn graceful_shutdown() {
        // given
        let mut lifecycle = MockPluginLifecycle::new(
            "shutdown-plugin",
            true,
            vec![healthy_server("alpha", &["search"])],
            DiscoveryResult {
                tools: vec![tool("search")],
                resources: Vec::new(),
                partial: false,
            },
            None,
        );

        // when
        let shutdown = lifecycle.shutdown();
        let post_shutdown = lifecycle.healthcheck();

        // then
        assert_eq!(shutdown, Ok(()));
        assert_eq!(PluginLifecycleEvent::Shutdown.to_string(), "shutdown");
        assert_eq!(post_shutdown.state, PluginState::Stopped);
    }
}
