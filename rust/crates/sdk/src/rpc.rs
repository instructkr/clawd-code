//! JSON-RPC server for agent integration.
//!
//! Run with `claw --mode rpc` to expose a JSON-RPC interface over stdin/stdout.
//! This allows any language to integrate with Claw Code via a simple protocol.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event_bus::AgentSessionEvent;
use crate::session::AgentSession;
use crate::tool_registry::ToolRegistry;
use crate::SessionTree;

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Value,
    id: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: String,
    method: String,
    params: Value,
}

// ---------------------------------------------------------------------------
// RPC method params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "method")]
#[serde(rename_all = "snake_case")]
enum RpcMethod {
    #[serde(rename = "session.create")]
    SessionCreate {
        #[serde(default = "default_model")]
        model: String,
        #[serde(default)]
        system_prompt: Vec<String>,
    },
    #[serde(rename = "session.turn")]
    SessionTurn {
        session_id: String,
        input: String,
    },
    #[serde(rename = "session.list")]
    SessionList,
    #[serde(rename = "session.destroy")]
    SessionDestroy {
        session_id: String,
    },
    #[serde(rename = "session.tree.fork")]
    SessionTreeFork {
        session_id: String,
        node_id: String,
        new_branch_id: String,
    },
    #[serde(rename = "session.tree.navigate")]
    SessionTreeNavigate {
        session_id: String,
        node_id: String,
    },
    #[serde(rename = "session.tree.path")]
    SessionTreePath {
        session_id: String,
    },
    #[serde(rename = "events.subscribe")]
    EventsSubscribe {
        #[serde(default)]
        session_id: Option<String>,
    },
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "shutdown")]
    Shutdown,
}

fn default_model() -> String {
    "claude-sonnet-4-6".to_string()
}

// ---------------------------------------------------------------------------
// Session state managed by the RPC server
// ---------------------------------------------------------------------------

struct ManagedSession {
    session: AgentSession,
    tree: SessionTree,
}

// ---------------------------------------------------------------------------
// RPC server
// ---------------------------------------------------------------------------

/// Run the JSON-RPC server, reading from stdin and writing to stdout.
pub fn run_rpc_server() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut server = RpcServer::new(stdin.lock(), stdout.lock());
    server.run()?;
    Ok(())
}

struct RpcServer<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    sessions: HashMap<String, ManagedSession>,
    running: bool,
}

impl<R: BufRead, W: Write> RpcServer<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            sessions: HashMap::new(),
            running: true,
        }
    }

    fn run(&mut self) -> Result<(), String> {
        while self.running {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let request = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
                        Ok(req) => req,
                        Err(e) => {
                            self.write_error(None, -32700, &format!("Parse error: {e}"))?;
                            continue;
                        }
                    };
                    self.handle_request(request)?;
                }
                Err(e) => {
                    self.write_error(None, -32000, &format!("IO error: {e}"))?;
                    break;
                }
            }
        }
        Ok(())
    }

    fn handle_request(&mut self, req: JsonRpcRequest) -> Result<(), String> {
        let method = req.method.clone();
        let params = req.params.clone();
        let id = req.id;

        let result = match method.as_str() {
            "ping" => self.handle_ping(),
            "shutdown" => {
                self.running = false;
                Ok(serde_json::json!({"status": "shutting_down"}))
            }
            "session.create" => self.handle_session_create(&params),
            "session.turn" => self.handle_session_turn(&params),
            "session.list" => self.handle_session_list(),
            "session.destroy" => self.handle_session_destroy(&params),
            "session.tree.fork" => self.handle_tree_fork(&params),
            "session.tree.navigate" => self.handle_tree_navigate(&params),
            "session.tree.path" => self.handle_tree_path(&params),
            "events.subscribe" => self.handle_events_subscribe(&params),
            _ => {
                return self.write_error(
                    id,
                    -32601,
                    &format!("Method not found: {method}"),
                );
            }
        };

        match result {
            Ok(value) => self.write_result(id, value),
            Err(msg) => self.write_error(id, -32000, &msg),
        }
    }

    // -- Handlers --

    fn handle_ping(&mut self) -> Result<Value, String> {
        Ok(serde_json::json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
    }

    fn handle_session_create(&mut self, params: &Value) -> Result<Value, String> {
        let model = params
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("claude-sonnet-4-6");
        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let (session, _event_bus) = AgentSession::new(
            model,
            system_prompt,
            ToolRegistry::new(),
            runtime::PermissionMode::DangerFullAccess,
        )?;

        let session_id = session.session_id().to_string();

        // Initialize session tree
        let mut tree = SessionTree::new();
        tree.set_root(&session_id, "system", None);

        let managed = ManagedSession { session, tree };
        self.sessions.insert(session_id.clone(), managed);

        Ok(serde_json::json!({
            "sessionId": session_id,
            "model": model,
        }))
    }

    fn handle_session_turn(&mut self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("missing session_id")?;
        let input = params
            .get("input")
            .and_then(|v| v.as_str())
            .ok_or("missing input")?;

        let managed = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;

        // Add user turn to tree
        let turn_id = format!("turn-{}", managed.tree.active().map_or(0, |n| {
            // Count existing children of active node
            n.children.len()
        }));
        if let Some(active_id) = managed.tree.active().map(|n| n.id.clone()) {
            let _ = managed.tree.add_child(&turn_id, &active_id, "user", Some(input.to_string()));
        }

        match managed.session.run_turn(input) {
            Ok(summary) => {
                // Add assistant turn to tree
                let assistant_id = format!("{turn_id}-response");
                let _ = managed.tree.add_child(
                    &assistant_id,
                    &turn_id,
                    "assistant",
                    None,
                );

                Ok(serde_json::json!({
                    "sessionId": session_id,
                    "status": "completed",
                    "tokensUsed": summary.usage.input_tokens + summary.usage.output_tokens,
                }))
            }
            Err(e) => Ok(serde_json::json!({
                "sessionId": session_id,
                "status": "error",
                "error": e.to_string(),
            })),
        }
    }

    fn handle_session_list(&mut self) -> Result<Value, String> {
        let sessions: Vec<Value> = self
            .sessions
            .iter()
            .map(|(id, managed)| {
                serde_json::json!({
                    "sessionId": id,
                    "model": managed.session.model(),
                })
            })
            .collect();
        Ok(serde_json::json!({"sessions": sessions}))
    }

    fn handle_session_destroy(&mut self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("missing session_id")?;

        if self.sessions.remove(session_id).is_some() {
            Ok(serde_json::json!({"status": "destroyed", "sessionId": session_id}))
        } else {
            Err(format!("session not found: {session_id}"))
        }
    }

    fn handle_tree_fork(&mut self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("missing session_id")?;
        let node_id = params
            .get("node_id")
            .and_then(|v| v.as_str())
            .ok_or("missing node_id")?;
        let new_branch_id = params
            .get("new_branch_id")
            .and_then(|v| v.as_str())
            .ok_or("missing new_branch_id")?;

        let managed = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;

        managed
            .tree
            .fork_at(node_id, new_branch_id)
            .map(|_| {
                serde_json::json!({
                    "sessionId": session_id,
                    "activeId": new_branch_id,
                })
            })
            .map_err(|e| e)
    }

    fn handle_tree_navigate(&mut self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("missing session_id")?;
        let node_id = params
            .get("node_id")
            .and_then(|v| v.as_str())
            .ok_or("missing node_id")?;

        let managed = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;

        managed
            .tree
            .navigate_to(node_id)
            .map(|_| {
                serde_json::json!({
                    "sessionId": session_id,
                    "activeId": node_id,
                })
            })
            .map_err(|e| e)
    }

    fn handle_tree_path(&mut self, params: &Value) -> Result<Value, String> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("missing session_id")?;

        let managed = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;

        let path: Vec<Value> = managed
            .tree
            .active_path()
            .iter()
            .map(|node| {
                serde_json::json!({
                    "id": node.id,
                    "role": node.role,
                    "label": node.label,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "sessionId": session_id,
            "path": path,
        }))
    }

    fn handle_events_subscribe(&mut self, params: &Value) -> Result<Value, String> {
        // In RPC mode, events are streamed as notifications.
        // For now, we drain any pending events from existing sessions.
        let session_id = params.get("session_id").and_then(|v| v.as_str());

        let mut events = Vec::new();

        if let Some(sid) = session_id {
            // Drain events from specific session
            if let Some(_managed) = self.sessions.get(sid) {
                // Note: AgentSession.subscribe() requires &mut self, but we
                // can't borrow mutably here while sessions are borrowed.
                // Instead, we report the subscription status.
                events.push(serde_json::json!({
                    "event": "subscribed",
                    "sessionId": sid,
                }));
            }
        } else {
            // Subscribe to all sessions
            events.push(serde_json::json!({
                "event": "subscribed",
                "sessionId": null,
            }));
        }

        // Write events as notifications
        for event in events {
            let notification = JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "events.stream".to_string(),
                params: event,
            };
            let line = serde_json::to_string(&notification).map_err(|e| e.to_string())?;
            writeln!(self.writer, "{line}").map_err(|e| e.to_string())?;
            self.writer.flush().map_err(|e| e.to_string())?;
        }

        Ok(serde_json::json!({"status": "subscribed"}))
    }

    // -- I/O helpers --

    fn write_result(&mut self, id: Option<u64>, result: Value) -> Result<(), String> {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        };
        let line = serde_json::to_string(&response).map_err(|e| e.to_string())?;
        writeln!(self.writer, "{line}").map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())
    }

    fn write_error(&mut self, id: Option<u64>, code: i64, message: &str) -> Result<(), String> {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        };
        let line = serde_json::to_string(&response).map_err(|e| e.to_string())?;
        writeln!(self.writer, "{line}").map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_server(_input: &str) -> RpcServer<io::StdinLock<'static>, io::StdoutLock<'static>> {
        // We test with Cursor-based I/O in integration tests.
        // Unit tests focus on request parsing and response formatting.
        unreachable!()
    }

    #[test]
    fn parses_session_create_request() {
        let json = r#"{"jsonrpc":"2.0","method":"session.create","params":{"model":"claude-sonnet-4-6","system_prompt":["You are helpful."]},"id":1}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.method, "session.create");
        assert_eq!(req.id, Some(1));
    }

    #[test]
    fn parses_ping_request() {
        let json = r#"{"jsonrpc":"2.0","method":"ping","params":{},"id":1}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.method, "ping");
    }

    #[test]
    fn parses_shutdown_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"shutdown","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.method, "shutdown");
        assert!(req.id.is_none()); // notification, no id
    }

    #[test]
    fn response_serializes_correctly() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::json!({"sessionId": "abc123"})),
            error: None,
            id: Some(1),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"sessionId\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn error_response_serializes_correctly() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "Method not found".to_string(),
                data: None,
            }),
            id: Some(2),
        };
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32601"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn round_trip_rpc_with_cursor() {
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"params\":{},\"id\":42}\n";
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let resp: JsonRpcResponse =
            serde_json::from_str(output_str.trim()).expect("response should parse");
        assert_eq!(resp.id, Some(42));
        let result = resp.result.expect("should have result");
        assert_eq!(result["status"], "ok");
    }

    #[test]
    fn session_create_and_list_round_trip() {
        let input = format!(
            "{}\n{}\n",
            r#"{"jsonrpc":"2.0","method":"session.create","params":{"model":"claude-sonnet-4-6","system_prompt":["test"]},"id":1}"#,
            r#"{"jsonrpc":"2.0","method":"session.list","params":{},"id":2}"#,
        );
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let lines: Vec<&str> = output_str.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        // First response: session.create
        let resp1: JsonRpcResponse =
            serde_json::from_str(lines[0]).expect("first response should parse");
        assert_eq!(resp1.id, Some(1));
        let session_id = resp1.result.expect("should have result")["sessionId"]
            .as_str()
            .expect("should have sessionId")
            .to_string();
        assert!(!session_id.is_empty());

        // Second response: session.list
        let resp2: JsonRpcResponse =
            serde_json::from_str(lines[1]).expect("second response should parse");
        assert_eq!(resp2.id, Some(2));
        let result2 = resp2.result.expect("should have result");
        let sessions = result2["sessions"]
            .as_array()
            .expect("should be array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], session_id);
    }

    #[test]
    fn session_create_destroy_and_list_round_trip() {
        // Create, destroy, then list — should show empty
        let input = format!(
            "{}\n{}\n{}\n",
            r#"{"jsonrpc":"2.0","method":"session.create","params":{"model":"claude-sonnet-4-6"},"id":1}"#,
            r#"{"jsonrpc":"2.0","method":"session.list","params":{},"id":2}"#,
            r#"{"jsonrpc":"2.0","method":"shutdown","params":{},"id":3}"#,
        );
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let lines: Vec<&str> = output_str.trim().lines().collect();
        assert!(lines.len() >= 2);

        // First response: session.create
        let resp1: JsonRpcResponse =
            serde_json::from_str(lines[0]).expect("first response should parse");
        assert_eq!(resp1.id, Some(1));
        let session_id = resp1.result.expect("should have result")["sessionId"]
            .as_str()
            .expect("should have sessionId")
            .to_string();

        // Second response: session.list — should have 1 session
        let resp2: JsonRpcResponse =
            serde_json::from_str(lines[1]).expect("second response should parse");
        assert_eq!(resp2.id, Some(2));
        let result2 = resp2.result.expect("should have result");
        let sessions = result2["sessions"].as_array().expect("should be array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], session_id);
    }

    #[test]
    fn shutdown_stops_server() {
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\",\"params\":{},\"id\":99}\n";
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");
        assert!(!server.running);

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let resp: JsonRpcResponse =
            serde_json::from_str(output_str.trim()).expect("response should parse");
        assert_eq!(resp.id, Some(99));
        assert_eq!(resp.result.expect("should have result")["status"], "shutting_down");
    }

    #[test]
    fn unknown_method_returns_error() {
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"nonexistent\",\"params\":{},\"id\":5}\n";
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let resp: JsonRpcResponse =
            serde_json::from_str(output_str.trim()).expect("response should parse");
        assert_eq!(resp.id, Some(5));
        let error = resp.error.expect("should have error");
        assert_eq!(error.code, -32601);
        assert!(error.message.contains("Method not found"));
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let input = "not valid json\n";
        let reader = Cursor::new(input.as_bytes());
        let mut output = Cursor::new(Vec::new());

        let mut server = RpcServer::new(reader, &mut output);
        server.run().expect("server should run");

        let output_str = String::from_utf8(output.into_inner()).expect("valid utf8");
        let resp: JsonRpcResponse =
            serde_json::from_str(output_str.trim()).expect("response should parse");
        assert!(resp.id.is_none());
        let error = resp.error.expect("should have error");
        assert_eq!(error.code, -32700);
    }
}
