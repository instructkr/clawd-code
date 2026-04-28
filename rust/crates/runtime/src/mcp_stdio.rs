use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::process::Stdio;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::config::{McpTransport, RuntimeConfig, ScopedMcpServerConfig};
use crate::mcp::mcp_tool_name;
use crate::mcp_client::{McpClientBootstrap, McpClientTransport, McpStdioTransport};
use crate::mcp_lifecycle_hardened::{
    McpDegradedReport, McpErrorSurface, McpFailedServer, McpLifecyclePhase,
};

#[cfg(test)]
const MCP_INITIALIZE_TIMEOUT_MS: u64 = 200;
#[cfg(not(test))]
const MCP_INITIALIZE_TIMEOUT_MS: u64 = 10_000;

#[cfg(test)]
const MCP_LIST_TOOLS_TIMEOUT_MS: u64 = 300;
#[cfg(not(test))]
const MCP_LIST_TOOLS_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(u64),
    String(String),
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcRequest<T = JsonValue> {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<T>,
}

impl<T> JsonRpcRequest<T> {
    #[must_use]
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: Option<T>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcResponse<T = JsonValue> {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeParams {
    pub protocol_version: String,
    pub capabilities: JsonValue,
    pub client_info: McpInitializeClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeResult {
    pub protocol_version: String,
    pub capabilities: JsonValue,
    pub server_info: McpInitializeServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpListToolsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<JsonValue>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpListToolsResult {
    pub tools: Vec<McpTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCallParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonValue>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolCallContent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub data: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCallResult {
    #[serde(default)]
    pub content: Vec<McpToolCallContent>,
    #[serde(default)]
    pub structured_content: Option<JsonValue>,
    #[serde(default)]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpListResourcesParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResource {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<JsonValue>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpListResourcesResult {
    pub resources: Vec<McpResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpReadResourceParams {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpReadResourceResult {
    pub contents: Vec<McpResourceContents>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagedMcpTool {
    pub server_name: String,
    pub qualified_name: String,
    pub raw_name: String,
    pub tool: McpTool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedMcpServer {
    pub server_name: String,
    pub transport: McpTransport,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDiscoveryFailure {
    pub server_name: String,
    pub phase: McpLifecyclePhase,
    pub error: String,
    pub recoverable: bool,
    pub context: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDiscoveryReport {
    pub tools: Vec<ManagedMcpTool>,
    pub failed_servers: Vec<McpDiscoveryFailure>,
    pub unsupported_servers: Vec<UnsupportedMcpServer>,
    pub degraded_startup: Option<McpDegradedReport>,
}

#[derive(Debug)]
pub enum McpServerManagerError {
    Io(io::Error),
    Transport {
        server_name: String,
        method: &'static str,
        source: io::Error,
    },
    JsonRpc {
        server_name: String,
        method: &'static str,
        error: JsonRpcError,
    },
    InvalidResponse {
        server_name: String,
        method: &'static str,
        details: String,
    },
    Timeout {
        server_name: String,
        method: &'static str,
        timeout_ms: u64,
    },
    UnknownTool {
        qualified_name: String,
    },
    UnknownServer {
        server_name: String,
    },
}

impl std::fmt::Display for McpServerManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Transport {
                server_name,
                method,
                source,
            } => write!(
                f,
                "MCP server `{server_name}` transport failed during {method}: {source}"
            ),
            Self::JsonRpc {
                server_name,
                method,
                error,
            } => write!(
                f,
                "MCP server `{server_name}` returned JSON-RPC error for {method}: {} ({})",
                error.message, error.code
            ),
            Self::InvalidResponse {
                server_name,
                method,
                details,
            } => write!(
                f,
                "MCP server `{server_name}` returned invalid response for {method}: {details}"
            ),
            Self::Timeout {
                server_name,
                method,
                timeout_ms,
            } => write!(
                f,
                "MCP server `{server_name}` timed out after {timeout_ms} ms while handling {method}"
            ),
            Self::UnknownTool { qualified_name } => {
                write!(f, "unknown MCP tool `{qualified_name}`")
            }
            Self::UnknownServer { server_name } => write!(f, "unknown MCP server `{server_name}`"),
        }
    }
}

impl std::error::Error for McpServerManagerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Transport { source, .. } => Some(source),
            Self::JsonRpc { .. }
            | Self::InvalidResponse { .. }
            | Self::Timeout { .. }
            | Self::UnknownTool { .. }
            | Self::UnknownServer { .. } => None,
        }
    }
}

impl From<io::Error> for McpServerManagerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl McpServerManagerError {
    fn lifecycle_phase(&self) -> McpLifecyclePhase {
        match self {
            Self::Io(_) => McpLifecyclePhase::SpawnConnect,
            Self::Transport { method, .. }
            | Self::JsonRpc { method, .. }
            | Self::InvalidResponse { method, .. }
            | Self::Timeout { method, .. } => lifecycle_phase_for_method(method),
            Self::UnknownTool { .. } => McpLifecyclePhase::ToolDiscovery,
            Self::UnknownServer { .. } => McpLifecyclePhase::ServerRegistration,
        }
    }

    fn recoverable(&self) -> bool {
        !matches!(
            self.lifecycle_phase(),
            McpLifecyclePhase::InitializeHandshake
        ) && matches!(self, Self::Transport { .. } | Self::Timeout { .. })
    }

    fn discovery_failure(&self, server_name: &str) -> McpDiscoveryFailure {
        let phase = self.lifecycle_phase();
        let recoverable = self.recoverable();
        let context = self.error_context();

        McpDiscoveryFailure {
            server_name: server_name.to_string(),
            phase,
            error: self.to_string(),
            recoverable,
            context,
        }
    }

    fn error_context(&self) -> BTreeMap<String, String> {
        match self {
            Self::Io(error) => BTreeMap::from([("kind".to_string(), error.kind().to_string())]),
            Self::Transport {
                server_name,
                method,
                source,
            } => BTreeMap::from([
                ("server".to_string(), server_name.clone()),
                ("method".to_string(), (*method).to_string()),
                ("io_kind".to_string(), source.kind().to_string()),
            ]),
            Self::JsonRpc {
                server_name,
                method,
                error,
            } => BTreeMap::from([
                ("server".to_string(), server_name.clone()),
                ("method".to_string(), (*method).to_string()),
                ("jsonrpc_code".to_string(), error.code.to_string()),
            ]),
            Self::InvalidResponse {
                server_name,
                method,
                details,
            } => BTreeMap::from([
                ("server".to_string(), server_name.clone()),
                ("method".to_string(), (*method).to_string()),
                ("details".to_string(), details.clone()),
            ]),
            Self::Timeout {
                server_name,
                method,
                timeout_ms,
            } => BTreeMap::from([
                ("server".to_string(), server_name.clone()),
                ("method".to_string(), (*method).to_string()),
                ("timeout_ms".to_string(), timeout_ms.to_string()),
            ]),
            Self::UnknownTool { qualified_name } => {
                BTreeMap::from([("qualified_tool".to_string(), qualified_name.clone())])
            }
            Self::UnknownServer { server_name } => {
                BTreeMap::from([("server".to_string(), server_name.clone())])
            }
        }
    }
}

fn lifecycle_phase_for_method(method: &str) -> McpLifecyclePhase {
    match method {
        "initialize" => McpLifecyclePhase::InitializeHandshake,
        "tools/list" => McpLifecyclePhase::ToolDiscovery,
        "resources/list" => McpLifecyclePhase::ResourceDiscovery,
        "resources/read" | "tools/call" => McpLifecyclePhase::Invocation,
        _ => McpLifecyclePhase::ErrorSurfacing,
    }
}

fn unsupported_server_failed_server(server: &UnsupportedMcpServer) -> McpFailedServer {
    McpFailedServer {
        server_name: server.server_name.clone(),
        phase: McpLifecyclePhase::ServerRegistration,
        error: McpErrorSurface::new(
            McpLifecyclePhase::ServerRegistration,
            Some(server.server_name.clone()),
            server.reason.clone(),
            BTreeMap::from([("transport".to_string(), format!("{:?}", server.transport))]),
            false,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRoute {
    server_name: String,
    raw_name: String,
}

#[derive(Debug)]
struct ManagedMcpServer {
    bootstrap: McpClientBootstrap,
    process: Option<McpStdioProcess>,
    initialized: bool,
}

impl ManagedMcpServer {
    fn new(bootstrap: McpClientBootstrap) -> Self {
        Self {
            bootstrap,
            process: None,
            initialized: false,
        }
    }
}

#[derive(Debug)]
pub struct McpServerManager {
    servers: BTreeMap<String, ManagedMcpServer>,
    unsupported_servers: Vec<UnsupportedMcpServer>,
    tool_index: BTreeMap<String, ToolRoute>,
    next_request_id: u64,
}

impl McpServerManager {
    #[must_use]
    pub fn from_runtime_config(config: &RuntimeConfig) -> Self {
        Self::from_servers(config.mcp().servers())
    }

    #[must_use]
    pub fn from_servers(servers: &BTreeMap<String, ScopedMcpServerConfig>) -> Self {
        let mut managed_servers = BTreeMap::new();
        let mut unsupported_servers = Vec::new();

        for (server_name, server_config) in servers {
            if server_config.transport() == McpTransport::Stdio {
                let bootstrap = McpClientBootstrap::from_scoped_config(server_name, server_config);
                managed_servers.insert(server_name.clone(), ManagedMcpServer::new(bootstrap));
            } else {
                unsupported_servers.push(UnsupportedMcpServer {
                    server_name: server_name.clone(),
                    transport: server_config.transport(),
                    reason: format!(
                        "transport {:?} is not supported by McpServerManager",
                        server_config.transport()
                    ),
                });
            }
        }

        Self {
            servers: managed_servers,
            unsupported_servers,
            tool_index: BTreeMap::new(),
            next_request_id: 1,
        }
    }

    #[must_use]
    pub fn unsupported_servers(&self) -> &[UnsupportedMcpServer] {
        &self.unsupported_servers
    }

    #[must_use]
    pub fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    pub async fn discover_tools(&mut self) -> Result<Vec<ManagedMcpTool>, McpServerManagerError> {
        let server_names = self.servers.keys().cloned().collect::<Vec<_>>();
        let mut discovered_tools = Vec::new();

        for server_name in server_names {
            let server_tools = self.discover_tools_for_server(&server_name).await?;
            self.clear_routes_for_server(&server_name);

            for tool in server_tools {
                self.tool_index.insert(
                    tool.qualified_name.clone(),
                    ToolRoute {
                        server_name: tool.server_name.clone(),
                        raw_name: tool.raw_name.clone(),
                    },
                );
                discovered_tools.push(tool);
            }
        }

        Ok(discovered_tools)
    }

    pub async fn discover_tools_best_effort(&mut self) -> McpToolDiscoveryReport {
        let server_names = self.server_names();
        let mut discovered_tools = Vec::new();
        let mut working_servers = Vec::new();
        let mut failed_servers = Vec::new();

        for server_name in server_names {
            match self.discover_tools_for_server(&server_name).await {
                Ok(server_tools) => {
                    working_servers.push(server_name.clone());
                    self.clear_routes_for_server(&server_name);
                    for tool in server_tools {
                        self.tool_index.insert(
                            tool.qualified_name.clone(),
                            ToolRoute {
                                server_name: tool.server_name.clone(),
                                raw_name: tool.raw_name.clone(),
                            },
                        );
                        discovered_tools.push(tool);
                    }
                }
                Err(error) => {
                    self.clear_routes_for_server(&server_name);
                    failed_servers.push(error.discovery_failure(&server_name));
                }
            }
        }

        let degraded_failed_servers = failed_servers
            .iter()
            .map(|failure| McpFailedServer {
                server_name: failure.server_name.clone(),
                phase: failure.phase,
                error: McpErrorSurface::new(
                    failure.phase,
                    Some(failure.server_name.clone()),
                    failure.error.clone(),
                    failure.context.clone(),
                    failure.recoverable,
                ),
            })
            .chain(
                self.unsupported_servers
                    .iter()
                    .map(unsupported_server_failed_server),
            )
            .collect::<Vec<_>>();
        let degraded_startup = (!working_servers.is_empty() && !degraded_failed_servers.is_empty())
            .then(|| {
                McpDegradedReport::new(
                    working_servers,
                    degraded_failed_servers,
                    discovered_tools
                        .iter()
                        .map(|tool| tool.qualified_name.clone())
                        .collect(),
                    Vec::new(),
                )
            });

        McpToolDiscoveryReport {
            tools: discovered_tools,
            failed_servers,
            unsupported_servers: self.unsupported_servers.clone(),
            degraded_startup,
        }
    }

    pub async fn call_tool(
        &mut self,
        qualified_tool_name: &str,
        arguments: Option<JsonValue>,
    ) -> Result<JsonRpcResponse<McpToolCallResult>, McpServerManagerError> {
        let route = self
            .tool_index
            .get(qualified_tool_name)
            .cloned()
            .ok_or_else(|| McpServerManagerError::UnknownTool {
                qualified_name: qualified_tool_name.to_string(),
            })?;

        let timeout_ms = self.tool_call_timeout_ms(&route.server_name)?;

        self.ensure_server_ready(&route.server_name).await?;
        let request_id = self.take_request_id();
        let response =
            {
                let server = self.server_mut(&route.server_name)?;
                let process = server.process.as_mut().ok_or_else(|| {
                    McpServerManagerError::InvalidResponse {
                        server_name: route.server_name.clone(),
                        method: "tools/call",
                        details: "server process missing after initialization".to_string(),
                    }
                })?;
                Self::run_process_request(
                    &route.server_name,
                    "tools/call",
                    timeout_ms,
                    process.call_tool(
                        request_id,
                        McpToolCallParams {
                            name: route.raw_name,
                            arguments,
                            meta: None,
                        },
                    ),
                )
                .await
            };

        if let Err(error) = &response {
            if Self::should_reset_server(error) {
                self.reset_server(&route.server_name).await?;
            }
        }

        response
    }

    pub async fn list_resources(
        &mut self,
        server_name: &str,
    ) -> Result<McpListResourcesResult, McpServerManagerError> {
        let mut attempts = 0;

        loop {
            match self.list_resources_once(server_name).await {
                Ok(resources) => return Ok(resources),
                Err(error) if attempts == 0 && Self::is_retryable_error(&error) => {
                    self.reset_server(server_name).await?;
                    attempts += 1;
                }
                Err(error) => {
                    if Self::should_reset_server(&error) {
                        self.reset_server(server_name).await?;
                    }
                    return Err(error);
                }
            }
        }
    }

    pub async fn read_resource(
        &mut self,
        server_name: &str,
        uri: &str,
    ) -> Result<McpReadResourceResult, McpServerManagerError> {
        let mut attempts = 0;

        loop {
            match self.read_resource_once(server_name, uri).await {
                Ok(resource) => return Ok(resource),
                Err(error) if attempts == 0 && Self::is_retryable_error(&error) => {
                    self.reset_server(server_name).await?;
                    attempts += 1;
                }
                Err(error) => {
                    if Self::should_reset_server(&error) {
                        self.reset_server(server_name).await?;
                    }
                    return Err(error);
                }
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), McpServerManagerError> {
        let server_names = self.servers.keys().cloned().collect::<Vec<_>>();
        for server_name in server_names {
            let server = self.server_mut(&server_name)?;
            if let Some(process) = server.process.as_mut() {
                process.shutdown().await?;
            }
            server.process = None;
            server.initialized = false;
        }
        Ok(())
    }

    fn clear_routes_for_server(&mut self, server_name: &str) {
        self.tool_index
            .retain(|_, route| route.server_name != server_name);
    }

    fn server_mut(
        &mut self,
        server_name: &str,
    ) -> Result<&mut ManagedMcpServer, McpServerManagerError> {
        self.servers
            .get_mut(server_name)
            .ok_or_else(|| McpServerManagerError::UnknownServer {
                server_name: server_name.to_string(),
            })
    }

    fn take_request_id(&mut self) -> JsonRpcId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        JsonRpcId::Number(id)
    }

    fn tool_call_timeout_ms(&self, server_name: &str) -> Result<u64, McpServerManagerError> {
        let server =
            self.servers
                .get(server_name)
                .ok_or_else(|| McpServerManagerError::UnknownServer {
                    server_name: server_name.to_string(),
                })?;
        match &server.bootstrap.transport {
            McpClientTransport::Stdio(transport) => Ok(transport.resolved_tool_call_timeout_ms()),
            other => Err(McpServerManagerError::InvalidResponse {
                server_name: server_name.to_string(),
                method: "tools/call",
                details: format!("unsupported MCP transport for stdio manager: {other:?}"),
            }),
        }
    }

    fn server_process_exited(&mut self, server_name: &str) -> Result<bool, McpServerManagerError> {
        let server = self.server_mut(server_name)?;
        match server.process.as_mut() {
            Some(process) => Ok(process.has_exited()?),
            None => Ok(false),
        }
    }

    async fn discover_tools_for_server(
        &mut self,
        server_name: &str,
    ) -> Result<Vec<ManagedMcpTool>, McpServerManagerError> {
        let mut attempts = 0;

        loop {
            match self.discover_tools_for_server_once(server_name).await {
                Ok(tools) => return Ok(tools),
                Err(error) if attempts == 0 && Self::is_retryable_error(&error) => {
                    self.reset_server(server_name).await?;
                    attempts += 1;
                }
                Err(error) => {
                    if Self::should_reset_server(&error) {
                        self.reset_server(server_name).await?;
                    }
                    return Err(error);
                }
            }
        }
    }

    async fn discover_tools_for_server_once(
        &mut self,
        server_name: &str,
    ) -> Result<Vec<ManagedMcpTool>, McpServerManagerError> {
        self.ensure_server_ready(server_name).await?;

        let mut discovered_tools = Vec::new();
        let mut cursor = None;
        loop {
            let request_id = self.take_request_id();
            let response = {
                let server = self.server_mut(server_name)?;
                let process = server.process.as_mut().ok_or_else(|| {
                    McpServerManagerError::InvalidResponse {
                        server_name: server_name.to_string(),
                        method: "tools/list",
                        details: "server process missing after initialization".to_string(),
                    }
                })?;
                Self::run_process_request(
                    server_name,
                    "tools/list",
                    MCP_LIST_TOOLS_TIMEOUT_MS,
                    process.list_tools(
                        request_id,
                        Some(McpListToolsParams {
                            cursor: cursor.clone(),
                        }),
                    ),
                )
                .await?
            };

            if let Some(error) = response.error {
                return Err(McpServerManagerError::JsonRpc {
                    server_name: server_name.to_string(),
                    method: "tools/list",
                    error,
                });
            }

            let result = response
                .result
                .ok_or_else(|| McpServerManagerError::InvalidResponse {
                    server_name: server_name.to_string(),
                    method: "tools/list",
                    details: "missing result payload".to_string(),
                })?;

            for tool in result.tools {
                let qualified_name = mcp_tool_name(server_name, &tool.name);
                discovered_tools.push(ManagedMcpTool {
                    server_name: server_name.to_string(),
                    qualified_name,
                    raw_name: tool.name.clone(),
                    tool,
                });
            }

            match result.next_cursor {
                Some(next_cursor) => cursor = Some(next_cursor),
                None => break,
            }
        }

        Ok(discovered_tools)
    }

    async fn list_resources_once(
        &mut self,
        server_name: &str,
    ) -> Result<McpListResourcesResult, McpServerManagerError> {
        self.ensure_server_ready(server_name).await?;

        let mut resources = Vec::new();
        let mut cursor = None;
        loop {
            let request_id = self.take_request_id();
            let response = {
                let server = self.server_mut(server_name)?;
                let process = server.process.as_mut().ok_or_else(|| {
                    McpServerManagerError::InvalidResponse {
                        server_name: server_name.to_string(),
                        method: "resources/list",
                        details: "server process missing after initialization".to_string(),
                    }
                })?;
                Self::run_process_request(
                    server_name,
                    "resources/list",
                    MCP_LIST_TOOLS_TIMEOUT_MS,
                    process.list_resources(
                        request_id,
                        Some(McpListResourcesParams {
                            cursor: cursor.clone(),
                        }),
                    ),
                )
                .await?
            };

            if let Some(error) = response.error {
                return Err(McpServerManagerError::JsonRpc {
                    server_name: server_name.to_string(),
                    method: "resources/list",
                    error,
                });
            }

            let result = response
                .result
                .ok_or_else(|| McpServerManagerError::InvalidResponse {
                    server_name: server_name.to_string(),
                    method: "resources/list",
                    details: "missing result payload".to_string(),
                })?;

            resources.extend(result.resources);

            match result.next_cursor {
                Some(next_cursor) => cursor = Some(next_cursor),
                None => break,
            }
        }

        Ok(McpListResourcesResult {
            resources,
            next_cursor: None,
        })
    }

    async fn read_resource_once(
        &mut self,
        server_name: &str,
        uri: &str,
    ) -> Result<McpReadResourceResult, McpServerManagerError> {
        self.ensure_server_ready(server_name).await?;

        let request_id = self.take_request_id();
        let response =
            {
                let server = self.server_mut(server_name)?;
                let process = server.process.as_mut().ok_or_else(|| {
                    McpServerManagerError::InvalidResponse {
                        server_name: server_name.to_string(),
                        method: "resources/read",
                        details: "server process missing after initialization".to_string(),
                    }
                })?;
                Self::run_process_request(
                    server_name,
                    "resources/read",
                    MCP_LIST_TOOLS_TIMEOUT_MS,
                    process.read_resource(
                        request_id,
                        McpReadResourceParams {
                            uri: uri.to_string(),
                        },
                    ),
                )
                .await?
            };

        if let Some(error) = response.error {
            return Err(McpServerManagerError::JsonRpc {
                server_name: server_name.to_string(),
                method: "resources/read",
                error,
            });
        }

        response
            .result
            .ok_or_else(|| McpServerManagerError::InvalidResponse {
                server_name: server_name.to_string(),
                method: "resources/read",
                details: "missing result payload".to_string(),
            })
    }

    async fn reset_server(&mut self, server_name: &str) -> Result<(), McpServerManagerError> {
        let mut process = {
            let server = self.server_mut(server_name)?;
            server.initialized = false;
            server.process.take()
        };

        if let Some(process) = process.as_mut() {
            let _ = process.shutdown().await;
        }

        Ok(())
    }

    fn is_retryable_error(error: &McpServerManagerError) -> bool {
        matches!(
            error,
            McpServerManagerError::Transport { .. } | McpServerManagerError::Timeout { .. }
        )
    }

    fn should_reset_server(error: &McpServerManagerError) -> bool {
        matches!(
            error,
            McpServerManagerError::Transport { .. }
                | McpServerManagerError::Timeout { .. }
                | McpServerManagerError::InvalidResponse { .. }
        )
    }

    async fn run_process_request<T, F>(
        server_name: &str,
        method: &'static str,
        timeout_ms: u64,
        future: F,
    ) -> Result<T, McpServerManagerError>
    where
        F: Future<Output = io::Result<T>>,
    {
        match timeout(Duration::from_millis(timeout_ms), future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
                Err(McpServerManagerError::InvalidResponse {
                    server_name: server_name.to_string(),
                    method,
                    details: error.to_string(),
                })
            }
            Ok(Err(source)) => Err(McpServerManagerError::Transport {
                server_name: server_name.to_string(),
                method,
                source,
            }),
            Err(_) => Err(McpServerManagerError::Timeout {
                server_name: server_name.to_string(),
                method,
                timeout_ms,
            }),
        }
    }

    async fn ensure_server_ready(
        &mut self,
        server_name: &str,
    ) -> Result<(), McpServerManagerError> {
        if self.server_process_exited(server_name)? {
            self.reset_server(server_name).await?;
        }

        let mut attempts = 0;
        loop {
            let needs_spawn = self
                .servers
                .get(server_name)
                .map(|server| server.process.is_none())
                .ok_or_else(|| McpServerManagerError::UnknownServer {
                    server_name: server_name.to_string(),
                })?;

            if needs_spawn {
                let server = self.server_mut(server_name)?;
                server.process = Some(spawn_mcp_stdio_process(&server.bootstrap)?);
                server.initialized = false;
            }

            let needs_initialize = self
                .servers
                .get(server_name)
                .map(|server| !server.initialized)
                .ok_or_else(|| McpServerManagerError::UnknownServer {
                    server_name: server_name.to_string(),
                })?;

            if !needs_initialize {
                return Ok(());
            }

            let request_id = self.take_request_id();
            let response = {
                let server = self.server_mut(server_name)?;
                let process = server.process.as_mut().ok_or_else(|| {
                    McpServerManagerError::InvalidResponse {
                        server_name: server_name.to_string(),
                        method: "initialize",
                        details: "server process missing before initialize".to_string(),
                    }
                })?;
                Self::run_process_request(
                    server_name,
                    "initialize",
                    MCP_INITIALIZE_TIMEOUT_MS,
                    process.initialize(request_id, default_initialize_params()),
                )
                .await
            };

            let response = match response {
                Ok(response) => response,
                Err(error) if attempts == 0 && Self::is_retryable_error(&error) => {
                    self.reset_server(server_name).await?;
                    attempts += 1;
                    continue;
                }
                Err(error) => {
                    if Self::should_reset_server(&error) {
                        self.reset_server(server_name).await?;
                    }
                    return Err(error);
                }
            };

            if let Some(error) = response.error {
                return Err(McpServerManagerError::JsonRpc {
                    server_name: server_name.to_string(),
                    method: "initialize",
                    error,
                });
            }

            if response.result.is_none() {
                let error = McpServerManagerError::InvalidResponse {
                    server_name: server_name.to_string(),
                    method: "initialize",
                    details: "missing result payload".to_string(),
                };
                self.reset_server(server_name).await?;
                return Err(error);
            }

            let server = self.server_mut(server_name)?;
            server.initialized = true;
            return Ok(());
        }
    }
}

#[derive(Debug)]
pub struct McpStdioProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpStdioProcess {
    pub fn spawn(transport: &McpStdioTransport) -> io::Result<Self> {
        let mut command = Command::new(&transport.command);
        command
            .args(&transport.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_env(&mut command, &transport.env);

        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("stdio MCP process missing stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("stdio MCP process missing stdout pipe"))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdin.write_all(bytes).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush().await
    }

    pub async fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.write_all(line.as_bytes()).await?;
        self.write_all(b"\n").await?;
        self.flush().await
    }

    pub async fn read_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        let bytes_read = self.stdout.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "MCP stdio stream closed while reading line",
            ));
        }
        Ok(line)
    }

    pub async fn read_available(&mut self) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0_u8; 4096];
        let read = self.stdout.read(&mut buffer).await?;
        buffer.truncate(read);
        Ok(buffer)
    }

    pub async fn write_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        let encoded = encode_frame(payload);
        self.write_all(&encoded).await?;
        self.flush().await
    }

    pub async fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let bytes_read = self.stdout.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MCP stdio stream closed while reading headers",
                ));
            }
            if line == "\r\n" {
                break;
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if let Some((name, value)) = header.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    let parsed = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    content_length = Some(parsed);
                }
            }
        }

        let content_length = content_length.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
        })?;
        let mut payload = vec![0_u8; content_length];
        self.stdout.read_exact(&mut payload).await?;
        Ok(payload)
    }

    pub async fn write_jsonrpc_message<T: Serialize>(&mut self, message: &T) -> io::Result<()> {
        let body = serde_json::to_vec(message)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.write_frame(&body).await
    }

    pub async fn read_jsonrpc_message<T: DeserializeOwned>(&mut self) -> io::Result<T> {
        let payload = self.read_frame().await?;
        serde_json::from_slice(&payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub async fn send_request<T: Serialize>(
        &mut self,
        request: &JsonRpcRequest<T>,
    ) -> io::Result<()> {
        self.write_jsonrpc_message(request).await
    }

    pub async fn read_response<T: DeserializeOwned>(&mut self) -> io::Result<JsonRpcResponse<T>> {
        self.read_jsonrpc_message().await
    }

    pub async fn request<TParams: Serialize, TResult: DeserializeOwned>(
        &mut self,
        id: JsonRpcId,
        method: impl Into<String>,
        params: Option<TParams>,
    ) -> io::Result<JsonRpcResponse<TResult>> {
        let method = method.into();
        let request = JsonRpcRequest::new(id.clone(), method.clone(), params);
        self.send_request(&request).await?;
        let response = self.read_response().await?;

        if response.jsonrpc != "2.0" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "MCP response for {method} used unsupported jsonrpc version `{}`",
                    response.jsonrpc
                ),
            ));
        }

        if response.id != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "MCP response for {method} used mismatched id: expected {id:?}, got {:?}",
                    response.id
                ),
            ));
        }

        Ok(response)
    }

    pub async fn initialize(
        &mut self,
        id: JsonRpcId,
        params: McpInitializeParams,
    ) -> io::Result<JsonRpcResponse<McpInitializeResult>> {
        self.request(id, "initialize", Some(params)).await
    }

    pub async fn list_tools(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListToolsParams>,
    ) -> io::Result<JsonRpcResponse<McpListToolsResult>> {
        self.request(id, "tools/list", params).await
    }

    pub async fn call_tool(
        &mut self,
        id: JsonRpcId,
        params: McpToolCallParams,
    ) -> io::Result<JsonRpcResponse<McpToolCallResult>> {
        self.request(id, "tools/call", Some(params)).await
    }

    pub async fn list_resources(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListResourcesParams>,
    ) -> io::Result<JsonRpcResponse<McpListResourcesResult>> {
        self.request(id, "resources/list", params).await
    }

    pub async fn read_resource(
        &mut self,
        id: JsonRpcId,
        params: McpReadResourceParams,
    ) -> io::Result<JsonRpcResponse<McpReadResourceResult>> {
        self.request(id, "resources/read", Some(params)).await
    }

    pub async fn terminate(&mut self) -> io::Result<()> {
        self.child.kill().await
    }

    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    pub fn has_exited(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            match self.child.kill().await {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => return Err(error),
            }
        }
        let _ = self.child.wait().await?;
        Ok(())
    }
}

pub fn spawn_mcp_stdio_process(bootstrap: &McpClientBootstrap) -> io::Result<McpStdioProcess> {
    match &bootstrap.transport {
        McpClientTransport::Stdio(transport) => McpStdioProcess::spawn(transport),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "MCP bootstrap transport for {} is not stdio: {other:?}",
                bootstrap.server_name
            ),
        )),
    }
}

fn apply_env(command: &mut Command, env: &BTreeMap<String, String>) {
    for (key, value) in env {
        command.env(key, value);
    }
}

fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    let mut framed = header.into_bytes();
    framed.extend_from_slice(payload);
    framed
}

fn default_initialize_params() -> McpInitializeParams {
    McpInitializeParams {
        protocol_version: "2025-03-26".to_string(),
        capabilities: JsonValue::Object(serde_json::Map::new()),
        client_info: McpInitializeClientInfo {
            name: "runtime".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tokio::runtime::Builder;

    use crate::config::{
        ConfigSource, McpRemoteServerConfig, McpSdkServerConfig, McpServerConfig,
        McpStdioServerConfig, McpWebSocketServerConfig, ScopedMcpServerConfig,
    };
    use crate::mcp::mcp_tool_name;
    use crate::mcp_client::McpClientBootstrap;

    use super::{
        spawn_mcp_stdio_process, unsupported_server_failed_server, JsonRpcId, JsonRpcRequest,
        JsonRpcResponse, McpInitializeClientInfo, McpInitializeParams, McpInitializeResult,
        McpInitializeServerInfo, McpListToolsResult, McpReadResourceParams, McpReadResourceResult,
        McpServerManager, McpServerManagerError, McpStdioProcess, McpTool, McpToolCallParams,
    };
    use crate::McpLifecyclePhase;

    fn temp_dir() -> PathBuf {
        static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let unique_id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("runtime-mcp-stdio-{nanos}-{unique_id}"))
    }

    fn write_echo_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("echo-mcp.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\nprintf 'READY:%s\\n' \"$MCP_TEST_TOKEN\"\nIFS= read -r line\nprintf 'ECHO:%s\\n' \"$line\"\n",
        )
        .expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    fn write_jsonrpc_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("jsonrpc-mcp.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, os, sys",
            "LOWERCASE_CONTENT_LENGTH = os.environ.get('MCP_LOWERCASE_CONTENT_LENGTH') == '1'",
            "MISMATCHED_RESPONSE_ID = os.environ.get('MCP_MISMATCHED_RESPONSE_ID') == '1'",
            "header = b''",
            r"while not header.endswith(b'\r\n\r\n'):",
            "    chunk = sys.stdin.buffer.read(1)",
            "    if not chunk:",
            "        raise SystemExit(1)",
            "    header += chunk",
            "length = 0",
            r"for line in header.decode().split('\r\n'):",
            r"    if line.lower().startswith('content-length:'):",
            r"        length = int(line.split(':', 1)[1].strip())",
            "payload = sys.stdin.buffer.read(length)",
            "request = json.loads(payload.decode())",
            r"assert request['jsonrpc'] == '2.0'",
            r"assert request['method'] == 'initialize'",
            "response_id = 'wrong-id' if MISMATCHED_RESPONSE_ID else request['id']",
            "header_name = 'content-length' if LOWERCASE_CONTENT_LENGTH else 'Content-Length'",
            r"response = json.dumps({",
            r"    'jsonrpc': '2.0',",
            r"    'id': response_id,",
            r"    'result': {",
            r"        'protocolVersion': request['params']['protocolVersion'],",
            r"        'capabilities': {'tools': {}},",
            r"        'serverInfo': {'name': 'fake-mcp', 'version': '0.1.0'}",
            r"    }",
            r"}).encode()",
            r"sys.stdout.buffer.write(f'{header_name}: {len(response)}\r\n\r\n'.encode() + response)",
            "sys.stdout.buffer.flush()",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    #[allow(clippy::too_many_lines)]
    fn write_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("fake-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, os, sys, time",
            "TOOL_CALL_DELAY_MS = int(os.environ.get('MCP_TOOL_CALL_DELAY_MS', '0'))",
            "INVALID_TOOL_CALL_RESPONSE = os.environ.get('MCP_INVALID_TOOL_CALL_RESPONSE') == '1'",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            r"            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    if method == 'initialize':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': request['params']['protocolVersion'],",
            "                'capabilities': {'tools': {}, 'resources': {}},",
            "                'serverInfo': {'name': 'fake-mcp', 'version': '0.2.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'tools': [",
            "                    {",
            "                        'name': 'echo',",
            "                        'description': 'Echoes text',",
            "                        'inputSchema': {",
            "                            'type': 'object',",
            "                            'properties': {'text': {'type': 'string'}},",
            "                            'required': ['text']",
            "                        }",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'tools/call':",
            "        if INVALID_TOOL_CALL_RESPONSE:",
            "            sys.stdout.buffer.write(b'Content-Length: 5\\r\\n\\r\\nnope!')",
            "            sys.stdout.buffer.flush()",
            "            continue",
            "        if TOOL_CALL_DELAY_MS:",
            "            time.sleep(TOOL_CALL_DELAY_MS / 1000)",
            "        args = request['params'].get('arguments') or {}",
            "        if request['params']['name'] == 'fail':",
            "            send_message({",
            "                'jsonrpc': '2.0',",
            "                'id': request['id'],",
            "                'error': {'code': -32001, 'message': 'tool failed'},",
            "            })",
            "        else:",
            "            text = args.get('text', '')",
            "            send_message({",
            "                'jsonrpc': '2.0',",
            "                'id': request['id'],",
            "                'result': {",
            "                    'content': [{'type': 'text', 'text': f'echo:{text}'}],",
            "                    'structuredContent': {'echoed': text},",
            "                    'isError': False",
            "                }",
            "            })",
            "    elif method == 'resources/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'resources': [",
            "                    {",
            "                        'uri': 'file://guide.txt',",
            "                        'name': 'guide',",
            "                        'description': 'Guide text',",
            "                        'mimeType': 'text/plain'",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'resources/read':",
            "        uri = request['params']['uri']",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'contents': [",
            "                    {",
            "                        'uri': uri,",
            "                        'mimeType': 'text/plain',",
            "                        'text': f'contents for {uri}'",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': f'unknown method: {method}'},",
            "        })",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    #[allow(clippy::too_many_lines)]
    fn write_manager_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("manager-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, os, sys, time",
            "",
            "LABEL = os.environ.get('MCP_SERVER_LABEL', 'server')",
            "LOG_PATH = os.environ.get('MCP_LOG_PATH')",
            "EXIT_AFTER_TOOLS_LIST = os.environ.get('MCP_EXIT_AFTER_TOOLS_LIST') == '1'",
            "FAIL_ONCE_MODE = os.environ.get('MCP_FAIL_ONCE_MODE')",
            "FAIL_ONCE_MARKER = os.environ.get('MCP_FAIL_ONCE_MARKER')",
            "initialize_count = 0",
            "",
            "def log(method):",
            "    if LOG_PATH:",
            "        with open(LOG_PATH, 'a', encoding='utf-8') as handle:",
            "            handle.write(f'{method}\\n')",
            "",
            "def should_fail_once():",
            "    if not FAIL_ONCE_MODE or not FAIL_ONCE_MARKER:",
            "        return False",
            "    if os.path.exists(FAIL_ONCE_MARKER):",
            "        return False",
            "    with open(FAIL_ONCE_MARKER, 'w', encoding='utf-8') as handle:",
            "        handle.write(FAIL_ONCE_MODE)",
            "    return True",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            r"            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    log(method)",
            "    if method == 'initialize':",
            "        if FAIL_ONCE_MODE == 'initialize_hang' and should_fail_once():",
            "            log('initialize-hang')",
            "            while True:",
            "                time.sleep(1)",
            "        initialize_count += 1",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': request['params']['protocolVersion'],",
            "                'capabilities': {'tools': {}},",
            "                'serverInfo': {'name': LABEL, 'version': '1.0.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'tools': [",
            "                    {",
            "                        'name': 'echo',",
            "                        'description': f'Echo tool for {LABEL}',",
            "                        'inputSchema': {",
            "                            'type': 'object',",
            "                            'properties': {'text': {'type': 'string'}},",
            "                            'required': ['text']",
            "                        }",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "        if EXIT_AFTER_TOOLS_LIST:",
            "            raise SystemExit(0)",
            "    elif method == 'tools/call':",
            "        if FAIL_ONCE_MODE == 'tool_call_disconnect' and should_fail_once():",
            "            log('tools/call-disconnect')",
            "            raise SystemExit(0)",
            "        args = request['params'].get('arguments') or {}",
            "        text = args.get('text', '')",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'content': [{'type': 'text', 'text': f'{LABEL}:{text}'}],",
            "                'structuredContent': {",
            "                    'server': LABEL,",
            "                    'echoed': text,",
            "                    'initializeCount': initialize_count",
            "                },",
            "                'isError': False",
            "            }",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': f'unknown method: {method}'},",
            "        })",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    fn sample_bootstrap(script_path: &Path) -> McpClientBootstrap {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "/bin/sh".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([("MCP_TEST_TOKEN".to_string(), "secret-value".to_string())]),
                tool_call_timeout_ms: None,
            }),
        };
        McpClientBootstrap::from_scoped_config("stdio server", &config)
    }

    fn script_transport(script_path: &Path) -> crate::mcp_client::McpStdioTransport {
        script_transport_with_env(script_path, BTreeMap::new())
    }

    fn script_transport_with_env(
        script_path: &Path,
        env: BTreeMap<String, String>,
    ) -> crate::mcp_client::McpStdioTransport {
        crate::mcp_client::McpStdioTransport {
            command: "python3".to_string(),
            args: vec![script_path.to_string_lossy().into_owned()],
            env,
            tool_call_timeout_ms: None,
        }
    }

    fn cleanup_script(script_path: &Path) {
        if let Err(error) = fs::remove_file(script_path) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "cleanup script: {error}"
            );
        }
        if let Err(error) = fs::remove_dir_all(script_path.parent().expect("script parent")) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "cleanup dir: {error}"
            );
        }
    }

    fn manager_server_config(
        script_path: &Path,
        label: &str,
        log_path: &Path,
    ) -> ScopedMcpServerConfig {
        manager_server_config_with_env(script_path, label, log_path, BTreeMap::new())
    }

    fn manager_server_config_with_env(
        script_path: &Path,
        label: &str,
        log_path: &Path,
        extra_env: BTreeMap<String, String>,
    ) -> ScopedMcpServerConfig {
        let mut env = BTreeMap::from([
            ("MCP_SERVER_LABEL".to_string(), label.to_string()),
            (
                "MCP_LOG_PATH".to_string(),
                log_path.to_string_lossy().into_owned(),
            ),
        ]);
        env.extend(extra_env);
        ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "python3".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env,
                tool_call_timeout_ms: None,
            }),
        }
    }

    #[test]
    fn spawns_stdio_process_and_round_trips_io() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_echo_script();
            let bootstrap = sample_bootstrap(&script_path);
            let mut process = spawn_mcp_stdio_process(&bootstrap).expect("spawn stdio process");

            let ready = process.read_line().await.expect("read ready");
            assert_eq!(ready, "READY:secret-value\n");

            process
                .write_line("ping from client")
                .await
                .expect("write line");

            let echoed = process.read_line().await.expect("read echo");
            assert_eq!(echoed, "ECHO:ping from client\n");

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn rejects_non_stdio_bootstrap() {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Sdk(crate::config::McpSdkServerConfig {
                name: "sdk-server".to_string(),
            }),
        };
        let bootstrap = McpClientBootstrap::from_scoped_config("sdk server", &config);
        let error = spawn_mcp_stdio_process(&bootstrap).expect_err("non-stdio should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn round_trips_initialize_request_and_response_over_stdio_frames() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");

            let response = process
                .initialize(
                    JsonRpcId::Number(1),
                    McpInitializeParams {
                        protocol_version: "2025-03-26".to_string(),
                        capabilities: json!({"roots": {}}),
                        client_info: McpInitializeClientInfo {
                            name: "runtime-tests".to_string(),
                            version: "0.1.0".to_string(),
                        },
                    },
                )
                .await
                .expect("initialize roundtrip");

            assert_eq!(response.id, JsonRpcId::Number(1));
            assert_eq!(response.error, None);
            assert_eq!(
                response.result,
                Some(McpInitializeResult {
                    protocol_version: "2025-03-26".to_string(),
                    capabilities: json!({"tools": {}}),
                    server_info: McpInitializeServerInfo {
                        name: "fake-mcp".to_string(),
                        version: "0.1.0".to_string(),
                    },
                })
            );

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn write_jsonrpc_request_emits_content_length_frame() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");
            let request = JsonRpcRequest::new(
                JsonRpcId::Number(7),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "runtime-tests", "version": "0.1.0"}
                })),
            );

            process.send_request(&request).await.expect("send request");
            let response: JsonRpcResponse<serde_json::Value> =
                process.read_response().await.expect("read response");

            assert_eq!(response.id, JsonRpcId::Number(7));
            assert_eq!(response.jsonrpc, "2.0");

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn given_lowercase_content_length_when_initialize_then_response_parses() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport_with_env(
                &script_path,
                BTreeMap::from([("MCP_LOWERCASE_CONTENT_LENGTH".to_string(), "1".to_string())]),
            );
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");

            let response = process
                .initialize(
                    JsonRpcId::Number(8),
                    McpInitializeParams {
                        protocol_version: "2025-03-26".to_string(),
                        capabilities: json!({"roots": {}}),
                        client_info: McpInitializeClientInfo {
                            name: "runtime-tests".to_string(),
                            version: "0.1.0".to_string(),
                        },
                    },
                )
                .await
                .expect("initialize roundtrip");

            assert_eq!(response.id, JsonRpcId::Number(8));
            assert_eq!(response.error, None);
            assert!(response.result.is_some());

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn given_mismatched_response_id_when_initialize_then_invalid_data_is_returned() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport_with_env(
                &script_path,
                BTreeMap::from([("MCP_MISMATCHED_RESPONSE_ID".to_string(), "1".to_string())]),
            );
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");

            let error = process
                .initialize(
                    JsonRpcId::Number(9),
                    McpInitializeParams {
                        protocol_version: "2025-03-26".to_string(),
                        capabilities: json!({"roots": {}}),
                        client_info: McpInitializeClientInfo {
                            name: "runtime-tests".to_string(),
                            version: "0.1.0".to_string(),
                        },
                    },
                )
                .await
                .expect_err("mismatched response id should fail");

            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert!(error.to_string().contains("mismatched id"));

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn direct_spawn_uses_transport_env() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_echo_script();
            let transport = crate::mcp_client::McpStdioTransport {
                command: "/bin/sh".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([("MCP_TEST_TOKEN".to_string(), "direct-secret".to_string())]),
                tool_call_timeout_ms: None,
            };
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");
            let ready = process.read_available().await.expect("read ready");
            assert_eq!(String::from_utf8_lossy(&ready), "READY:direct-secret\n");
            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn lists_tools_calls_tool_and_reads_resources_over_jsonrpc() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn fake mcp server");

            let tools = process
                .list_tools(JsonRpcId::Number(2), None)
                .await
                .expect("list tools");
            assert_eq!(tools.error, None);
            assert_eq!(tools.id, JsonRpcId::Number(2));
            assert_eq!(
                tools.result,
                Some(McpListToolsResult {
                    tools: vec![McpTool {
                        name: "echo".to_string(),
                        description: Some("Echoes text".to_string()),
                        input_schema: Some(json!({
                            "type": "object",
                            "properties": {"text": {"type": "string"}},
                            "required": ["text"]
                        })),
                        annotations: None,
                        meta: None,
                    }],
                    next_cursor: None,
                })
            );

            let call = process
                .call_tool(
                    JsonRpcId::String("call-1".to_string()),
                    McpToolCallParams {
                        name: "echo".to_string(),
                        arguments: Some(json!({"text": "hello"})),
                        meta: None,
                    },
                )
                .await
                .expect("call tool");
            assert_eq!(call.error, None);
            let call_result = call.result.expect("tool result");
            assert_eq!(call_result.is_error, Some(false));
            assert_eq!(
                call_result.structured_content,
                Some(json!({"echoed": "hello"}))
            );
            assert_eq!(call_result.content.len(), 1);
            assert_eq!(call_result.content[0].kind, "text");
            assert_eq!(
                call_result.content[0].data.get("text"),
                Some(&json!("echo:hello"))
            );

            let resources = process
                .list_resources(JsonRpcId::Number(3), None)
                .await
                .expect("list resources");
            let resources_result = resources.result.expect("resources result");
            assert_eq!(resources_result.resources.len(), 1);
            assert_eq!(resources_result.resources[0].uri, "file://guide.txt");
            assert_eq!(
                resources_result.resources[0].mime_type.as_deref(),
                Some("text/plain")
            );

            let read = process
                .read_resource(
                    JsonRpcId::Number(4),
                    McpReadResourceParams {
                        uri: "file://guide.txt".to_string(),
                    },
                )
                .await
                .expect("read resource");
            assert_eq!(
                read.result,
                Some(McpReadResourceResult {
                    contents: vec![super::McpResourceContents {
                        uri: "file://guide.txt".to_string(),
                        mime_type: Some("text/plain".to_string()),
                        text: Some("contents for file://guide.txt".to_string()),
                        blob: None,
                        meta: None,
                    }],
                })
            );

            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn surfaces_jsonrpc_errors_from_tool_calls() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn fake mcp server");

            let response = process
                .call_tool(
                    JsonRpcId::Number(9),
                    McpToolCallParams {
                        name: "fail".to_string(),
                        arguments: None,
                        meta: None,
                    },
                )
                .await
                .expect("call tool with error response");

            assert_eq!(response.id, JsonRpcId::Number(9));
            assert!(response.result.is_none());
            assert_eq!(response.error.as_ref().map(|e| e.code), Some(-32001));
            assert_eq!(
                response.error.as_ref().map(|e| e.message.as_str()),
                Some("tool failed")
            );

            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_discovers_tools_from_stdio_config() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");

            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].server_name, "alpha");
            assert_eq!(tools[0].raw_name, "echo");
            assert_eq!(tools[0].qualified_name, mcp_tool_name("alpha", "echo"));
            assert_eq!(tools[0].tool.name, "echo");
            assert!(manager.unsupported_servers().is_empty());

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_routes_tool_calls_to_correct_server() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let alpha_log = root.join("alpha.log");
            let beta_log = root.join("beta.log");
            let servers = BTreeMap::from([
                (
                    "alpha".to_string(),
                    manager_server_config(&script_path, "alpha", &alpha_log),
                ),
                (
                    "beta".to_string(),
                    manager_server_config(&script_path, "beta", &beta_log),
                ),
            ]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");
            assert_eq!(tools.len(), 2);

            let alpha = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "hello"})),
                )
                .await
                .expect("call alpha tool");
            let beta = manager
                .call_tool(
                    &mcp_tool_name("beta", "echo"),
                    Some(json!({"text": "world"})),
                )
                .await
                .expect("call beta tool");

            assert_eq!(
                alpha
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("server")),
                Some(&json!("alpha"))
            );
            assert_eq!(
                beta.result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("server")),
                Some(&json!("beta"))
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_times_out_slow_tool_calls() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("timeout.log");
            let servers = BTreeMap::from([(
                "slow".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Stdio(McpStdioServerConfig {
                        command: "python3".to_string(),
                        args: vec![script_path.to_string_lossy().into_owned()],
                        env: BTreeMap::from([(
                            "MCP_TOOL_CALL_DELAY_MS".to_string(),
                            "200".to_string(),
                        )]),
                        tool_call_timeout_ms: Some(25),
                    }),
                },
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let error = manager
                .call_tool(
                    &mcp_tool_name("slow", "echo"),
                    Some(json!({"text": "slow"})),
                )
                .await
                .expect_err("slow tool call should time out");

            match error {
                McpServerManagerError::Timeout {
                    server_name,
                    method,
                    timeout_ms,
                } => {
                    assert_eq!(server_name, "slow");
                    assert_eq!(method, "tools/call");
                    assert_eq!(timeout_ms, 25);
                }
                other => panic!("expected timeout error, got {other:?}"),
            }

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
            let _ = fs::remove_file(log_path);
        });
    }

    #[test]
    fn manager_surfaces_parse_errors_from_tool_calls() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let servers = BTreeMap::from([(
                "broken".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Stdio(McpStdioServerConfig {
                        command: "python3".to_string(),
                        args: vec![script_path.to_string_lossy().into_owned()],
                        env: BTreeMap::from([(
                            "MCP_INVALID_TOOL_CALL_RESPONSE".to_string(),
                            "1".to_string(),
                        )]),
                        tool_call_timeout_ms: Some(1_000),
                    }),
                },
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let error = manager
                .call_tool(
                    &mcp_tool_name("broken", "echo"),
                    Some(json!({"text": "invalid-json"})),
                )
                .await
                .expect_err("invalid json should fail");

            match error {
                McpServerManagerError::InvalidResponse {
                    server_name,
                    method,
                    details,
                } => {
                    assert_eq!(server_name, "broken");
                    assert_eq!(method, "tools/call");
                    assert!(
                        details.contains("expected ident") || details.contains("expected value")
                    );
                }
                other => panic!("expected invalid response error, got {other:?}"),
            }

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn given_child_exits_after_discovery_when_calling_twice_then_second_call_succeeds_after_reset()
    {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("dropping.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config_with_env(
                    &script_path,
                    "alpha",
                    &log_path,
                    BTreeMap::from([("MCP_EXIT_AFTER_TOOLS_LIST".to_string(), "1".to_string())]),
                ),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let first_error = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "reconnect"})),
                )
                .await
                .expect_err("first call should fail after transport drops");

            match first_error {
                McpServerManagerError::Transport {
                    server_name,
                    method,
                    source,
                } => {
                    assert_eq!(server_name, "alpha");
                    assert_eq!(method, "tools/call");
                    assert_eq!(source.kind(), ErrorKind::UnexpectedEof);
                }
                other => panic!("expected transport error, got {other:?}"),
            }

            let response = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "reconnect"})),
                )
                .await
                .expect("second tool call should succeed after reset");

            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("server")),
                Some(&json!("alpha"))
            );
            let log = fs::read_to_string(&log_path).expect("read log");
            assert_eq!(
                log.lines().collect::<Vec<_>>(),
                vec!["initialize", "tools/list", "initialize", "tools/call"]
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn given_initialize_hangs_once_when_discover_tools_then_manager_retries_and_succeeds() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("initialize-hang.log");
            let marker_path = root.join("initialize-hang.marker");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config_with_env(
                    &script_path,
                    "alpha",
                    &log_path,
                    BTreeMap::from([
                        (
                            "MCP_FAIL_ONCE_MODE".to_string(),
                            "initialize_hang".to_string(),
                        ),
                        (
                            "MCP_FAIL_ONCE_MARKER".to_string(),
                            marker_path.to_string_lossy().into_owned(),
                        ),
                    ]),
                ),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager
                .discover_tools()
                .await
                .expect("discover tools after retry");

            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].qualified_name, mcp_tool_name("alpha", "echo"));
            let log = fs::read_to_string(&log_path).expect("read log");
            assert_eq!(
                log.lines().collect::<Vec<_>>(),
                vec!["initialize", "initialize-hang", "initialize", "tools/list"]
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn given_tool_call_disconnects_once_when_calling_twice_then_manager_resets_and_next_call_succeeds(
    ) {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("tool-call-disconnect.log");
            let marker_path = root.join("tool-call-disconnect.marker");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config_with_env(
                    &script_path,
                    "alpha",
                    &log_path,
                    BTreeMap::from([
                        (
                            "MCP_FAIL_ONCE_MODE".to_string(),
                            "tool_call_disconnect".to_string(),
                        ),
                        (
                            "MCP_FAIL_ONCE_MARKER".to_string(),
                            marker_path.to_string_lossy().into_owned(),
                        ),
                    ]),
                ),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let first_error = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "first"})),
                )
                .await
                .expect_err("first tool call should fail when transport drops");

            match first_error {
                McpServerManagerError::Transport {
                    server_name,
                    method,
                    source,
                } => {
                    assert_eq!(server_name, "alpha");
                    assert_eq!(method, "tools/call");
                    assert_eq!(source.kind(), ErrorKind::UnexpectedEof);
                }
                other => panic!("expected transport error, got {other:?}"),
            }

            let response = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "second"})),
                )
                .await
                .expect("second tool call should succeed after reset");

            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("echoed")),
                Some(&json!("second"))
            );
            let log = fs::read_to_string(&log_path).expect("read log");
            assert_eq!(
                log.lines().collect::<Vec<_>>(),
                vec![
                    "initialize",
                    "tools/list",
                    "tools/call",
                    "tools/call-disconnect",
                    "initialize",
                    "tools/call",
                ]
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_lists_and_reads_resources_from_stdio_servers() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("resources.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let listed = manager
                .list_resources("alpha")
                .await
                .expect("list resources");
            assert_eq!(listed.resources.len(), 1);
            assert_eq!(listed.resources[0].uri, "file://guide.txt");

            let read = manager
                .read_resource("alpha", "file://guide.txt")
                .await
                .expect("read resource");
            assert_eq!(read.contents.len(), 1);
            assert_eq!(
                read.contents[0].text.as_deref(),
                Some("contents for file://guide.txt")
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    fn write_initialize_disconnect_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("initialize-disconnect.py");
        let script = [
            "#!/usr/bin/env python3",
            "import sys",
            "header = b''",
            r"while not header.endswith(b'\r\n\r\n'):",
            "    chunk = sys.stdin.buffer.read(1)",
            "    if not chunk:",
            "        raise SystemExit(1)",
            "    header += chunk",
            "length = 0",
            r"for line in header.decode().split('\r\n'):",
            r"    if line.lower().startswith('content-length:'):",
            r"        length = int(line.split(':', 1)[1].strip())",
            "if length:",
            "    sys.stdin.buffer.read(length)",
            "raise SystemExit(0)",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    #[test]
    fn manager_discovery_report_keeps_healthy_servers_when_one_server_fails() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let alpha_log = root.join("alpha.log");
            let broken_script_path = write_initialize_disconnect_script();
            let servers = BTreeMap::from([
                (
                    "alpha".to_string(),
                    manager_server_config(&script_path, "alpha", &alpha_log),
                ),
                (
                    "broken".to_string(),
                    ScopedMcpServerConfig {
                        scope: ConfigSource::Local,
                        config: McpServerConfig::Stdio(McpStdioServerConfig {
                            command: broken_script_path.display().to_string(),
                            args: Vec::new(),
                            env: BTreeMap::new(),
                            tool_call_timeout_ms: None,
                        }),
                    },
                ),
            ]);
            let mut manager = McpServerManager::from_servers(&servers);

            let report = manager.discover_tools_best_effort().await;

            assert_eq!(report.tools.len(), 1);
            assert_eq!(
                report.tools[0].qualified_name,
                mcp_tool_name("alpha", "echo")
            );
            assert_eq!(report.failed_servers.len(), 1);
            assert_eq!(report.failed_servers[0].server_name, "broken");
            assert_eq!(
                report.failed_servers[0].phase,
                McpLifecyclePhase::InitializeHandshake
            );
            assert!(!report.failed_servers[0].recoverable);
            assert_eq!(
                report.failed_servers[0]
                    .context
                    .get("method")
                    .map(String::as_str),
                Some("initialize")
            );
            assert!(report.failed_servers[0].error.contains("initialize"));
            let degraded = report
                .degraded_startup
                .as_ref()
                .expect("partial startup should surface degraded report");
            assert_eq!(degraded.working_servers, vec!["alpha".to_string()]);
            assert_eq!(degraded.failed_servers.len(), 1);
            assert_eq!(degraded.failed_servers[0].server_name, "broken");
            assert_eq!(
                degraded.failed_servers[0].phase,
                McpLifecyclePhase::InitializeHandshake
            );
            assert_eq!(
                degraded.available_tools,
                vec![mcp_tool_name("alpha", "echo")]
            );
            assert!(degraded.missing_tools.is_empty());

            let response = manager
                .call_tool(&mcp_tool_name("alpha", "echo"), Some(json!({"text": "ok"})))
                .await
                .expect("healthy server should remain callable");
            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("echoed")),
                Some(&json!("ok"))
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
            cleanup_script(&broken_script_path);
        });
    }

    #[test]
    fn manager_records_unsupported_non_stdio_servers_without_panicking() {
        let servers = BTreeMap::from([
            (
                "http".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Http(McpRemoteServerConfig {
                        url: "https://example.test/mcp".to_string(),
                        headers: BTreeMap::new(),
                        headers_helper: None,
                        oauth: None,
                    }),
                },
            ),
            (
                "sdk".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Sdk(McpSdkServerConfig {
                        name: "sdk-server".to_string(),
                    }),
                },
            ),
            (
                "ws".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Ws(McpWebSocketServerConfig {
                        url: "wss://example.test/mcp".to_string(),
                        headers: BTreeMap::new(),
                        headers_helper: None,
                    }),
                },
            ),
        ]);

        let manager = McpServerManager::from_servers(&servers);
        let unsupported = manager.unsupported_servers();

        assert_eq!(unsupported.len(), 3);
        assert_eq!(unsupported[0].server_name, "http");
        assert_eq!(unsupported[1].server_name, "sdk");
        assert_eq!(unsupported[2].server_name, "ws");
        assert_eq!(
            unsupported_server_failed_server(&unsupported[0]).phase,
            McpLifecyclePhase::ServerRegistration
        );
    }

    #[test]
    fn manager_shutdown_terminates_spawned_children_and_is_idempotent() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            manager.shutdown().await.expect("first shutdown");
            manager.shutdown().await.expect("second shutdown");

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_reuses_spawned_server_between_discovery_and_call() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let response = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "reuse"})),
                )
                .await
                .expect("call tool");

            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("initializeCount")),
                Some(&json!(1))
            );

            let log = fs::read_to_string(&log_path).expect("read log");
            assert_eq!(log.lines().filter(|line| *line == "initialize").count(), 1);
            assert_eq!(
                log.lines().collect::<Vec<_>>(),
                vec!["initialize", "tools/list", "tools/call"]
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_reports_unknown_qualified_tool_name() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let error = manager
                .call_tool(
                    &mcp_tool_name("alpha", "missing"),
                    Some(json!({"text": "nope"})),
                )
                .await
                .expect_err("unknown qualified tool should fail");

            match error {
                McpServerManagerError::UnknownTool { qualified_name } => {
                    assert_eq!(qualified_name, mcp_tool_name("alpha", "missing"));
                }
                other => panic!("expected unknown tool error, got {other:?}"),
            }

            cleanup_script(&script_path);
        });
    }
}
