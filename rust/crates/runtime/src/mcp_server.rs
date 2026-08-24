//! Minimal Model Context Protocol (MCP) server.
//!
//! Implements a newline-safe, LSP-framed JSON-RPC server over stdio that
//! answers `initialize`, `tools/list`, and `tools/call` requests. The framing
//! matches the client transport implemented in [`crate::mcp_stdio`] so this
//! server can be driven by either an external MCP client (e.g. Claude
//! Desktop) or `claw`'s own [`McpServerManager`](crate::McpServerManager).
//!
//! The server is intentionally small: it exposes a list of pre-built
//! [`McpTool`] descriptors and delegates `tools/call` to a caller-supplied
//! handler. Tool execution itself lives in the `tools` crate; this module is
//! purely the transport + dispatch loop.
//!
//! [`McpTool`]: crate::mcp_stdio::McpTool

use std::io;

use serde_json::{json, Value as JsonValue};
use tokio::io::{
    stdin, stdout, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout,
};

use crate::mcp_stdio::{
    JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse, McpInitializeResult,
    McpInitializeServerInfo, McpListToolsResult, McpTool, McpToolCallContent, McpToolCallParams,
    McpToolCallResult,
};

/// Protocol version the server advertises during `initialize`.
///
/// Matches the version used by the built-in client in
/// [`crate::mcp_stdio`], so the two stay in lockstep.
pub const MCP_SERVER_PROTOCOL_VERSION: &str = "2025-03-26";

/// Synchronous handler invoked for every `tools/call` request.
///
/// Returning `Ok(text)` yields a single `text` content block and
/// `isError: false`. Returning `Err(message)` yields a `text` block with the
/// error and `isError: true`, mirroring the error-surfacing convention used
/// elsewhere in claw.
pub type ToolCallHandler =
    Box<dyn Fn(&str, &JsonValue) -> Result<String, String> + Send + Sync + 'static>;

/// Configuration for an [`McpServer`] instance.
///
/// Named `McpServerSpec` rather than `McpServerConfig` to avoid colliding
/// with the existing client-side [`crate::config::McpServerConfig`] that
/// describes *remote* MCP servers the runtime connects to.
pub struct McpServerSpec {
    /// Name advertised in the `serverInfo` field of the `initialize` response.
    pub server_name: String,
    /// Version advertised in the `serverInfo` field of the `initialize`
    /// response.
    pub server_version: String,
    /// Tool descriptors returned for `tools/list`.
    pub tools: Vec<McpTool>,
    /// Handler invoked for `tools/call`.
    pub tool_handler: ToolCallHandler,
}

/// Minimal MCP stdio server.
///
/// The server runs a blocking read/dispatch/write loop over the current
/// process's stdin/stdout, terminating cleanly when the peer closes the
/// stream.
pub struct McpServer {
    spec: McpServerSpec,
    stdin: BufReader<Stdin>,
    stdout: Stdout,
}

impl McpServer {
    #[must_use]
    pub fn new(spec: McpServerSpec) -> Self {
        Self {
            spec,
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        }
    }

    /// Runs the server until the client closes stdin.
    ///
    /// Returns `Ok(())` on clean EOF; any other I/O error is propagated so
    /// callers can log and exit non-zero.
    pub async fn run(&mut self) -> io::Result<()> {
        loop {
            let Some(payload) = read_frame(&mut self.stdin).await? else {
                return Ok(());
            };

            // Requests and notifications share a wire format; the absence of
            // `id` distinguishes notifications, which must never receive a
            // response.
            let message: JsonValue = match serde_json::from_slice(&payload) {
                Ok(value) => value,
                Err(error) => {
                    // Parse error with null id per JSON-RPC 2.0 §4.2.
                    let response = JsonRpcResponse::<JsonValue> {
                        jsonrpc: "2.0".to_string(),
                        id: JsonRpcId::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("parse error: {error}"),
                            data: None,
                        }),
                    };
                    write_response(&mut self.stdout, &response).await?;
                    continue;
                }
            };

            if message.get("id").is_none() {
                // Notification: dispatch for side effects only (e.g. log),
                // but send no reply.
                continue;
            }

            let request: JsonRpcRequest<JsonValue> = match serde_json::from_value(message) {
                Ok(request) => request,
                Err(error) => {
                    let response = JsonRpcResponse::<JsonValue> {
                        jsonrpc: "2.0".to_string(),
                        id: JsonRpcId::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32600,
                            message: format!("invalid request: {error}"),
                            data: None,
                        }),
                    };
                    write_response(&mut self.stdout, &response).await?;
                    continue;
                }
            };

            let response = self.dispatch(request);
            write_response(&mut self.stdout, &response).await?;
        }
    }

    fn dispatch(&self, request: JsonRpcRequest<JsonValue>) -> JsonRpcResponse<JsonValue> {
        let id = request.id.clone();
        match request.method.as_str() {
            "initialize" => self.handle_initialize(id),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(id, request.params),
            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("method not found: {other}"),
                    data: None,
                }),
            },
        }
    }

    fn handle_initialize(&self, id: JsonRpcId) -> JsonRpcResponse<JsonValue> {
        let result = McpInitializeResult {
            protocol_version: MCP_SERVER_PROTOCOL_VERSION.to_string(),
            capabilities: json!({ "tools": {} }),
            server_info: McpInitializeServerInfo {
                name: self.spec.server_name.clone(),
                version: self.spec.server_version.clone(),
            },
        };
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: serde_json::to_value(result).ok(),
            error: None,
        }
    }

    fn handle_tools_list(&self, id: JsonRpcId) -> JsonRpcResponse<JsonValue> {
        let result = McpListToolsResult {
            tools: self.spec.tools.clone(),
            next_cursor: None,
        };
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: serde_json::to_value(result).ok(),
            error: None,
        }
    }

    fn handle_tools_call(
        &self,
        id: JsonRpcId,
        params: Option<JsonValue>,
    ) -> JsonRpcResponse<JsonValue> {
        let Some(params) = params else {
            return invalid_params_response(id, "missing params for tools/call");
        };
        let call: McpToolCallParams = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return invalid_params_response(id, &format!("invalid tools/call params: {error}"));
            }
        };
        let arguments = call.arguments.unwrap_or_else(|| json!({}));
        let tool_result = (self.spec.tool_handler)(&call.name, &arguments);
        let (text, is_error) = match tool_result {
            Ok(text) => (text, false),
            Err(message) => (message, true),
        };
        let mut data = std::collections::BTreeMap::new();
        data.insert("text".to_string(), JsonValue::String(text));
        let call_result = McpToolCallResult {
            content: vec![McpToolCallContent {
                kind: "text".to_string(),
                data,
            }],
            structured_content: None,
            is_error: Some(is_error),
            meta: None,
        };
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: serde_json::to_value(call_result).ok(),
            error: None,
        }
    }
}

fn invalid_params_response(id: JsonRpcId, message: &str) -> JsonRpcResponse<JsonValue> {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code: -32602,
            message: message.to_string(),
            data: None,
        }),
    }
}

/// Reads a single LSP-framed JSON-RPC payload from `reader`.
///
/// Returns `Ok(None)` on clean EOF before any header bytes have been read,
/// matching how [`crate::mcp_stdio::McpStdioProcess`] treats stream closure.
async fn read_frame(reader: &mut BufReader<Stdin>) -> io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut first_header = true;
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            if first_header {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "MCP stdio stream closed while reading headers",
            ));
        }
        first_header = false;
        if line == "\r\n" || line == "\n" {
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
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_response(
    stdout: &mut Stdout,
    response: &JsonRpcResponse<JsonValue>,
) -> io::Result<()> {
    let body = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdout.write_all(header.as_bytes()).await?;
    stdout.write_all(&body).await?;
    stdout.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_initialize_returns_server_info() {
        let server = McpServer {
            spec: McpServerSpec {
                server_name: "test".to_string(),
                server_version: "9.9.9".to_string(),
                tools: Vec::new(),
                tool_handler: Box::new(|_, _| Ok(String::new())),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(1),
            method: "initialize".to_string(),
            params: None,
        };
        let response = server.dispatch(request);
        assert_eq!(response.id, JsonRpcId::Number(1));
        assert!(response.error.is_none());
        let result = response.result.expect("initialize result");
        assert_eq!(result["protocolVersion"], MCP_SERVER_PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "test");
        assert_eq!(result["serverInfo"]["version"], "9.9.9");
    }

    #[test]
    fn dispatch_tools_list_returns_registered_tools() {
        let tool = McpTool {
            name: "echo".to_string(),
            description: Some("Echo".to_string()),
            input_schema: Some(json!({"type": "object"})),
            annotations: None,
            meta: None,
        };
        let server = McpServer {
            spec: McpServerSpec {
                server_name: "test".to_string(),
                server_version: "0.0.0".to_string(),
                tools: vec![tool.clone()],
                tool_handler: Box::new(|_, _| Ok(String::new())),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(2),
            method: "tools/list".to_string(),
            params: None,
        };
        let response = server.dispatch(request);
        assert!(response.error.is_none());
        let result = response.result.expect("tools/list result");
        assert_eq!(result["tools"][0]["name"], "echo");
    }

    #[test]
    fn dispatch_tools_call_wraps_handler_output() {
        let server = McpServer {
            spec: McpServerSpec {
                server_name: "test".to_string(),
                server_version: "0.0.0".to_string(),
                tools: Vec::new(),
                tool_handler: Box::new(|name, args| Ok(format!("called {name} with {args}"))),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(3),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "echo",
                "arguments": {"text": "hi"}
            })),
        };
        let response = server.dispatch(request);
        assert!(response.error.is_none());
        let result = response.result.expect("tools/call result");
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("called echo"));
    }

    #[test]
    fn dispatch_tools_call_surfaces_handler_error() {
        let server = McpServer {
            spec: McpServerSpec {
                server_name: "test".to_string(),
                server_version: "0.0.0".to_string(),
                tools: Vec::new(),
                tool_handler: Box::new(|_, _| Err("boom".to_string())),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(4),
            method: "tools/call".to_string(),
            params: Some(json!({"name": "broken"})),
        };
        let response = server.dispatch(request);
        let result = response.result.expect("tools/call result");
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "boom");
    }

    #[test]
    fn dispatch_unknown_method_returns_method_not_found() {
        let server = McpServer {
            spec: McpServerSpec {
                server_name: "test".to_string(),
                server_version: "0.0.0".to_string(),
                tools: Vec::new(),
                tool_handler: Box::new(|_, _| Ok(String::new())),
            },
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
        };
        let request = JsonRpcRequest::<JsonValue> {
            jsonrpc: "2.0".to_string(),
            id: JsonRpcId::Number(5),
            method: "nonsense".to_string(),
            params: None,
        };
        let response = server.dispatch(request);
        let error = response.error.expect("error payload");
        assert_eq!(error.code, -32601);
    }
}
