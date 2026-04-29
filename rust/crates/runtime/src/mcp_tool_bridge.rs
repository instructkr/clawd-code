#![allow(
    clippy::await_holding_lock,
    clippy::doc_markdown,
    clippy::match_same_arms,
    clippy::must_use_candidate,
    clippy::uninlined_format_args,
    clippy::unnested_or_patterns
)]
//! Bridge between MCP tool surface (ListMcpResources, ReadMcpResource, McpAuth, MCP)
//! and the existing McpServerManager runtime.
//!
//! Provides a stateful client registry that tool handlers can use to
//! connect to MCP servers and invoke their capabilities.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::mcp::mcp_tool_name;
use crate::mcp_stdio::McpServerManager;
use serde::{Deserialize, Serialize};

/// Status of a managed MCP server connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    AuthRequired,
    Error,
}

impl std::fmt::Display for McpConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::AuthRequired => write!(f, "auth_required"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Metadata about an MCP resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Metadata about an MCP tool exposed by a server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<serde_json::Value>,
}

/// Tracked state of an MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerState {
    pub server_name: String,
    pub status: McpConnectionStatus,
    pub tools: Vec<McpToolInfo>,
    pub resources: Vec<McpResourceInfo>,
    pub server_info: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct McpToolRegistry {
    inner: Arc<Mutex<HashMap<String, McpServerState>>>,
    manager: Arc<OnceLock<Arc<Mutex<McpServerManager>>>>,
}

impl McpToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_manager(
        &self,
        manager: Arc<Mutex<McpServerManager>>,
    ) -> Result<(), Arc<Mutex<McpServerManager>>> {
        self.manager.set(manager)
    }

    pub fn register_server(
        &self,
        server_name: &str,
        status: McpConnectionStatus,
        tools: Vec<McpToolInfo>,
        resources: Vec<McpResourceInfo>,
        server_info: Option<String>,
    ) {
        let mut inner = self.inner.lock().expect("mcp registry lock poisoned");
        inner.insert(
            server_name.to_owned(),
            McpServerState {
                server_name: server_name.to_owned(),
                status,
                tools,
                resources,
                server_info,
                error_message: None,
            },
        );
    }

    pub fn get_server(&self, server_name: &str) -> Option<McpServerState> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        inner.get(server_name).cloned()
    }

    pub fn list_servers(&self) -> Vec<McpServerState> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        inner.values().cloned().collect()
    }

    pub fn list_resources(&self, server_name: &str) -> Result<Vec<McpResourceInfo>, String> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        match inner.get(server_name) {
            Some(state) => {
                if state.status != McpConnectionStatus::Connected {
                    return Err(format!(
                        "server '{}' is not connected (status: {})",
                        server_name, state.status
                    ));
                }
                Ok(state.resources.clone())
            }
            None => Err(format!("server '{}' not found", server_name)),
        }
    }

    pub fn read_resource(&self, server_name: &str, uri: &str) -> Result<McpResourceInfo, String> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        let state = inner
            .get(server_name)
            .ok_or_else(|| format!("server '{}' not found", server_name))?;

        if state.status != McpConnectionStatus::Connected {
            return Err(format!(
                "server '{}' is not connected (status: {})",
                server_name, state.status
            ));
        }

        state
            .resources
            .iter()
            .find(|r| r.uri == uri)
            .cloned()
            .ok_or_else(|| format!("resource '{}' not found on server '{}'", uri, server_name))
    }

    pub fn list_tools(&self, server_name: &str) -> Result<Vec<McpToolInfo>, String> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        match inner.get(server_name) {
            Some(state) => {
                if state.status != McpConnectionStatus::Connected {
                    return Err(format!(
                        "server '{}' is not connected (status: {})",
                        server_name, state.status
                    ));
                }
                Ok(state.tools.clone())
            }
            None => Err(format!("server '{}' not found", server_name)),
        }
    }

    fn spawn_tool_call(
        manager: Arc<Mutex<McpServerManager>>,
        qualified_tool_name: String,
        arguments: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let join_handle = std::thread::Builder::new()
            .name(format!("mcp-tool-call-{qualified_tool_name}"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("failed to create MCP tool runtime: {error}"))?;

                runtime.block_on(async move {
                    let response = {
                        let mut manager = manager
                            .lock()
                            .map_err(|_| "mcp server manager lock poisoned".to_string())?;
                        manager
                            .discover_tools()
                            .await
                            .map_err(|error| error.to_string())?;
                        let response = manager
                            .call_tool(&qualified_tool_name, arguments)
                            .await
                            .map_err(|error| error.to_string());
                        let shutdown = manager.shutdown().await.map_err(|error| error.to_string());

                        match (response, shutdown) {
                            (Ok(response), Ok(())) => Ok(response),
                            (Err(error), Ok(())) | (Err(error), Err(_)) => Err(error),
                            (Ok(_), Err(error)) => Err(error),
                        }
                    }?;

                    if let Some(error) = response.error {
                        return Err(format!(
                            "MCP server returned JSON-RPC error for tools/call: {} ({})",
                            error.message, error.code
                        ));
                    }

                    let result = response.result.ok_or_else(|| {
                        "MCP server returned no result for tools/call".to_string()
                    })?;

                    serde_json::to_value(result)
                        .map_err(|error| format!("failed to serialize MCP tool result: {error}"))
                })
            })
            .map_err(|error| format!("failed to spawn MCP tool call thread: {error}"))?;

        join_handle.join().map_err(|panic_payload| {
            if let Some(message) = panic_payload.downcast_ref::<&str>() {
                format!("MCP tool call thread panicked: {message}")
            } else if let Some(message) = panic_payload.downcast_ref::<String>() {
                format!("MCP tool call thread panicked: {message}")
            } else {
                "MCP tool call thread panicked".to_string()
            }
        })?
    }

    pub fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        let state = inner
            .get(server_name)
            .ok_or_else(|| format!("server '{}' not found", server_name))?;

        if state.status != McpConnectionStatus::Connected {
            return Err(format!(
                "server '{}' is not connected (status: {})",
                server_name, state.status
            ));
        }

        if !state.tools.iter().any(|t| t.name == tool_name) {
            return Err(format!(
                "tool '{}' not found on server '{}'",
                tool_name, server_name
            ));
        }

        drop(inner);

        let manager = self
            .manager
            .get()
            .cloned()
            .ok_or_else(|| "MCP server manager is not configured".to_string())?;

        Self::spawn_tool_call(
            manager,
            mcp_tool_name(server_name, tool_name),
            (!arguments.is_null()).then(|| arguments.clone()),
        )
    }

    /// Set auth status for a server.
    pub fn set_auth_status(
        &self,
        server_name: &str,
        status: McpConnectionStatus,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("mcp registry lock poisoned");
        let state = inner
            .get_mut(server_name)
            .ok_or_else(|| format!("server '{}' not found", server_name))?;
        state.status = status;
        Ok(())
    }

    /// Disconnect / remove a server.
    pub fn disconnect(&self, server_name: &str) -> Option<McpServerState> {
        let mut inner = self.inner.lock().expect("mcp registry lock poisoned");
        inner.remove(server_name)
    }

    /// Number of registered servers.
    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("mcp registry lock poisoned");
        inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::config::{
        ConfigSource, McpServerConfig, McpStdioServerConfig, ScopedMcpServerConfig,
    };

    fn temp_dir() -> PathBuf {
        static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let unique_id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("runtime-mcp-tool-bridge-{nanos}-{unique_id}"))
    }

    fn cleanup_script(script_path: &Path) {
        if let Some(root) = script_path.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }

    fn write_bridge_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("bridge-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, os, sys",
            "LABEL = os.environ.get('MCP_SERVER_LABEL', 'server')",
            "LOG_PATH = os.environ.get('MCP_LOG_PATH')",
            "",
            "def log(method):",
            "    if LOG_PATH:",
            "        with open(LOG_PATH, 'a', encoding='utf-8') as handle:",
            "            handle.write(f'{method}\\n')",
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
            "    elif method == 'tools/call':",
            "        args = request['params'].get('arguments') or {}",
            "        text = args.get('text', '')",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'content': [{'type': 'text', 'text': f'{LABEL}:{text}'}],",
            "                'structuredContent': {'server': LABEL, 'echoed': text},",
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

    fn manager_server_config(
        script_path: &Path,
        server_name: &str,
        log_path: &Path,
    ) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "python3".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([
                    ("MCP_SERVER_LABEL".to_string(), server_name.to_string()),
                    (
                        "MCP_LOG_PATH".to_string(),
                        log_path.to_string_lossy().into_owned(),
                    ),
                ]),
                tool_call_timeout_ms: Some(1_000),
            }),
        }
    }

    #[test]
    fn registers_and_retrieves_server() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "test-server",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "greet".into(),
                description: Some("Greet someone".into()),
                input_schema: None,
            }],
            vec![McpResourceInfo {
                uri: "res://data".into(),
                name: "Data".into(),
                description: None,
                mime_type: Some("application/json".into()),
            }],
            Some("TestServer v1.0".into()),
        );

        let server = registry.get_server("test-server").expect("should exist");
        assert_eq!(server.status, McpConnectionStatus::Connected);
        assert_eq!(server.tools.len(), 1);
        assert_eq!(server.resources.len(), 1);
    }

    #[test]
    fn lists_resources_from_connected_server() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![],
            vec![McpResourceInfo {
                uri: "res://alpha".into(),
                name: "Alpha".into(),
                description: None,
                mime_type: None,
            }],
            None,
        );

        let resources = registry.list_resources("srv").expect("should succeed");
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, "res://alpha");
    }

    #[test]
    fn rejects_resource_listing_for_disconnected_server() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::Disconnected,
            vec![],
            vec![],
            None,
        );
        assert!(registry.list_resources("srv").is_err());
    }

    #[test]
    fn reads_specific_resource() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![],
            vec![McpResourceInfo {
                uri: "res://data".into(),
                name: "Data".into(),
                description: Some("Test data".into()),
                mime_type: Some("text/plain".into()),
            }],
            None,
        );

        let resource = registry
            .read_resource("srv", "res://data")
            .expect("should find");
        assert_eq!(resource.name, "Data");

        assert!(registry.read_resource("srv", "res://missing").is_err());
    }

    #[test]
    fn given_connected_server_without_manager_when_calling_tool_then_it_errors() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "greet".into(),
                description: None,
                input_schema: None,
            }],
            vec![],
            None,
        );

        let error = registry
            .call_tool("srv", "greet", &serde_json::json!({"name": "world"}))
            .expect_err("should require a configured manager");
        assert!(error.contains("MCP server manager is not configured"));

        // Unknown tool should fail
        assert!(registry
            .call_tool("srv", "missing", &serde_json::json!({}))
            .is_err());
    }

    #[test]
    fn given_connected_server_with_manager_when_calling_tool_then_it_returns_live_result() {
        let script_path = write_bridge_mcp_server_script();
        let root = script_path.parent().expect("script parent");
        let log_path = root.join("bridge.log");
        let servers = BTreeMap::from([(
            "alpha".to_string(),
            manager_server_config(&script_path, "alpha", &log_path),
        )]);
        let manager = Arc::new(Mutex::new(McpServerManager::from_servers(&servers)));

        let registry = McpToolRegistry::new();
        registry.register_server(
            "alpha",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "echo".into(),
                description: Some("Echo tool for alpha".into()),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                })),
            }],
            vec![],
            Some("bridge test server".into()),
        );
        registry
            .set_manager(Arc::clone(&manager))
            .expect("manager should only be set once");

        let result = registry
            .call_tool("alpha", "echo", &serde_json::json!({"text": "hello"}))
            .expect("should return live MCP result");

        assert_eq!(
            result["structuredContent"]["server"],
            serde_json::json!("alpha")
        );
        assert_eq!(
            result["structuredContent"]["echoed"],
            serde_json::json!("hello")
        );
        assert_eq!(
            result["content"][0]["text"],
            serde_json::json!("alpha:hello")
        );

        let log = fs::read_to_string(&log_path).expect("read log");
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            vec!["initialize", "tools/list", "tools/call"]
        );

        cleanup_script(&script_path);
    }

    #[test]
    fn rejects_tool_call_on_disconnected_server() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::AuthRequired,
            vec![McpToolInfo {
                name: "greet".into(),
                description: None,
                input_schema: None,
            }],
            vec![],
            None,
        );

        assert!(registry
            .call_tool("srv", "greet", &serde_json::json!({}))
            .is_err());
    }

    #[test]
    fn sets_auth_and_disconnects() {
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::AuthRequired,
            vec![],
            vec![],
            None,
        );

        registry
            .set_auth_status("srv", McpConnectionStatus::Connected)
            .expect("should succeed");
        let state = registry.get_server("srv").unwrap();
        assert_eq!(state.status, McpConnectionStatus::Connected);

        let removed = registry.disconnect("srv");
        assert!(removed.is_some());
        assert!(registry.is_empty());
    }

    #[test]
    fn rejects_operations_on_missing_server() {
        let registry = McpToolRegistry::new();
        assert!(registry.list_resources("missing").is_err());
        assert!(registry.read_resource("missing", "uri").is_err());
        assert!(registry.list_tools("missing").is_err());
        assert!(registry
            .call_tool("missing", "tool", &serde_json::json!({}))
            .is_err());
        assert!(registry
            .set_auth_status("missing", McpConnectionStatus::Connected)
            .is_err());
    }

    #[test]
    fn mcp_connection_status_display_all_variants() {
        // given
        let cases = [
            (McpConnectionStatus::Disconnected, "disconnected"),
            (McpConnectionStatus::Connecting, "connecting"),
            (McpConnectionStatus::Connected, "connected"),
            (McpConnectionStatus::AuthRequired, "auth_required"),
            (McpConnectionStatus::Error, "error"),
        ];

        // when
        let rendered: Vec<_> = cases
            .into_iter()
            .map(|(status, expected)| (status.to_string(), expected))
            .collect();

        // then
        assert_eq!(
            rendered,
            vec![
                ("disconnected".to_string(), "disconnected"),
                ("connecting".to_string(), "connecting"),
                ("connected".to_string(), "connected"),
                ("auth_required".to_string(), "auth_required"),
                ("error".to_string(), "error"),
            ]
        );
    }

    #[test]
    fn list_servers_returns_all_registered() {
        // given
        let registry = McpToolRegistry::new();
        registry.register_server(
            "alpha",
            McpConnectionStatus::Connected,
            vec![],
            vec![],
            None,
        );
        registry.register_server(
            "beta",
            McpConnectionStatus::Connecting,
            vec![],
            vec![],
            None,
        );

        // when
        let servers = registry.list_servers();

        // then
        assert_eq!(servers.len(), 2);
        assert!(servers.iter().any(|server| server.server_name == "alpha"));
        assert!(servers.iter().any(|server| server.server_name == "beta"));
    }

    #[test]
    fn list_tools_from_connected_server() {
        // given
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "inspect".into(),
                description: Some("Inspect data".into()),
                input_schema: Some(serde_json::json!({"type": "object"})),
            }],
            vec![],
            None,
        );

        // when
        let tools = registry.list_tools("srv").expect("tools should list");

        // then
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "inspect");
    }

    #[test]
    fn list_tools_rejects_disconnected_server() {
        // given
        let registry = McpToolRegistry::new();
        registry.register_server(
            "srv",
            McpConnectionStatus::AuthRequired,
            vec![],
            vec![],
            None,
        );

        // when
        let result = registry.list_tools("srv");

        // then
        let error = result.expect_err("non-connected server should fail");
        assert!(error.contains("not connected"));
        assert!(error.contains("auth_required"));
    }

    #[test]
    fn list_tools_rejects_missing_server() {
        // given
        let registry = McpToolRegistry::new();

        // when
        let result = registry.list_tools("missing");

        // then
        assert_eq!(
            result.expect_err("missing server should fail"),
            "server 'missing' not found"
        );
    }

    #[test]
    fn get_server_returns_none_for_missing() {
        // given
        let registry = McpToolRegistry::new();

        // when
        let server = registry.get_server("missing");

        // then
        assert!(server.is_none());
    }

    #[test]
    fn call_tool_payload_structure() {
        let script_path = write_bridge_mcp_server_script();
        let root = script_path.parent().expect("script parent");
        let log_path = root.join("payload.log");
        let servers = BTreeMap::from([(
            "srv".to_string(),
            manager_server_config(&script_path, "srv", &log_path),
        )]);
        let registry = McpToolRegistry::new();
        let arguments = serde_json::json!({"text": "world"});
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "echo".into(),
                description: Some("Echo tool for srv".into()),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                })),
            }],
            vec![],
            None,
        );
        registry
            .set_manager(Arc::new(Mutex::new(McpServerManager::from_servers(
                &servers,
            ))))
            .expect("manager should only be set once");

        let result = registry
            .call_tool("srv", "echo", &arguments)
            .expect("tool should return live payload");

        assert_eq!(result["structuredContent"]["server"], "srv");
        assert_eq!(result["structuredContent"]["echoed"], "world");
        assert_eq!(result["content"][0]["text"], "srv:world");

        cleanup_script(&script_path);
    }

    #[test]
    fn upsert_overwrites_existing_server() {
        // given
        let registry = McpToolRegistry::new();
        registry.register_server("srv", McpConnectionStatus::Connecting, vec![], vec![], None);

        // when
        registry.register_server(
            "srv",
            McpConnectionStatus::Connected,
            vec![McpToolInfo {
                name: "inspect".into(),
                description: None,
                input_schema: None,
            }],
            vec![],
            Some("Inspector".into()),
        );
        let state = registry.get_server("srv").expect("server should exist");

        // then
        assert_eq!(state.status, McpConnectionStatus::Connected);
        assert_eq!(state.tools.len(), 1);
        assert_eq!(state.server_info.as_deref(), Some("Inspector"));
    }

    #[test]
    fn disconnect_missing_returns_none() {
        // given
        let registry = McpToolRegistry::new();

        // when
        let removed = registry.disconnect("missing");

        // then
        assert!(removed.is_none());
    }

    #[test]
    fn len_and_is_empty_transitions() {
        // given
        let registry = McpToolRegistry::new();

        // when
        registry.register_server(
            "alpha",
            McpConnectionStatus::Connected,
            vec![],
            vec![],
            None,
        );
        registry.register_server("beta", McpConnectionStatus::Connected, vec![], vec![], None);
        let after_create = registry.len();
        registry.disconnect("alpha");
        let after_first_remove = registry.len();
        registry.disconnect("beta");

        // then
        assert_eq!(after_create, 2);
        assert_eq!(after_first_remove, 1);
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }
}
