use std::io;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LspId {
    Number(u64),
    String(String),
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspRequest {
    pub jsonrpc: String,
    pub id: LspId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<JsonValue>,
}

impl LspRequest {
    pub fn new(id: LspId, method: impl Into<String>, params: Option<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<JsonValue>,
}

impl LspNotification {
    pub fn new(method: impl Into<String>, params: Option<JsonValue>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspResponse {
    pub jsonrpc: String,
    pub id: LspId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LspError>,
}

impl LspResponse {
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn into_result(self) -> Result<JsonValue, LspError> {
        if let Some(error) = self.error {
            Err(error)
        } else {
            Ok(self.result.unwrap_or(JsonValue::Null))
        }
    }
}

/// A message received from an LSP server — either a response to a request
/// or a server-initiated notification (e.g. `textDocument/publishDiagnostics`).
#[derive(Debug, Clone)]
pub enum LspServerMessage {
    Response(LspResponse),
    Notification(LspNotification),
}

#[derive(Debug)]
pub enum LspTransportError {
    Io(io::Error),
    Timeout { method: String, timeout: Duration },
    JsonRpc(LspError),
    InvalidResponse { method: String, details: String },
    ServerExited,
}

impl std::fmt::Display for LspTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Timeout { method, timeout } => {
                write!(f, "LSP request `{method}` timed out after {}s", timeout.as_secs())
            }
            Self::JsonRpc(error) => {
                write!(f, "LSP JSON-RPC error: {} ({})", error.message, error.code)
            }
            Self::InvalidResponse { method, details } => {
                write!(f, "LSP invalid response for `{method}`: {details}")
            }
            Self::ServerExited => write!(f, "LSP server process exited unexpectedly"),
        }
    }
}

impl std::error::Error for LspTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::JsonRpc(_) | Self::Timeout { .. } | Self::InvalidResponse { .. } | Self::ServerExited => None,
        }
    }
}

impl From<io::Error> for LspTransportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
pub struct LspTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    request_timeout: Duration,
    pending_notifications: Vec<LspNotification>,
}

impl LspTransport {
    pub fn spawn(command: &str, args: &[String]) -> io::Result<Self> {
        Self::spawn_with_timeout(command, args, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn spawn_with_timeout(
        command: &str,
        args: &[String],
        request_timeout: Duration,
    ) -> io::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("LSP process missing stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("LSP process missing stdout pipe"))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            request_timeout,
            pending_notifications: Vec::new(),
        })
    }

    /// Construct an `LspTransport` from an already-spawned child process.
    /// Primarily useful for testing.
    #[cfg(test)]
    fn from_child(mut child: Child, request_timeout: Duration) -> Self {
        let stdin = child
            .stdin
            .take()
            .expect("LSP process missing stdin pipe");
        let stdout = child
            .stdout
            .take()
            .expect("LSP process missing stdout pipe");
        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            request_timeout,
            pending_notifications: Vec::new(),
        }
    }

    fn allocate_id(&mut self) -> LspId {
        let id = self.next_id;
        self.next_id += 1;
        LspId::Number(id)
    }

    pub async fn send_notification(
        &mut self,
        method: &str,
        params: Option<JsonValue>,
    ) -> Result<(), LspTransportError> {
        let notification = LspNotification::new(method, params);
        let body = serde_json::to_vec(&notification)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.write_frame(&body).await
    }

    pub async fn send_request(
        &mut self,
        method: &str,
        params: Option<JsonValue>,
    ) -> Result<LspResponse, LspTransportError> {
        let id = self.allocate_id();
        self.send_request_with_id(method, params, id).await
    }

    pub async fn send_request_with_id(
        &mut self,
        method: &str,
        params: Option<JsonValue>,
        id: LspId,
    ) -> Result<LspResponse, LspTransportError> {
        let request = LspRequest::new(id.clone(), method, params);
        let body = serde_json::to_vec(&request)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.write_frame(&body).await?;

        let method_owned = method.to_string();
        let timeout_duration = self.request_timeout;
        let response = match timeout(timeout_duration, async {
            loop {
                match self.read_message().await {
                    Ok(LspServerMessage::Response(r)) => break Ok(r),
                    Ok(LspServerMessage::Notification(n)) => {
                        self.pending_notifications.push(n);
                    }
                    Err(e) => break Err(e),
                }
            }
        })
        .await
        {
            Ok(inner) => inner,
            Err(_) => {
                return Err(LspTransportError::Timeout {
                    method: method_owned,
                    timeout: timeout_duration,
                })
            }
        }?;

        if response.jsonrpc != "2.0" {
            return Err(LspTransportError::InvalidResponse {
                method: method.to_string(),
                details: format!("unsupported jsonrpc version `{}`", response.jsonrpc),
            });
        }

        if response.id != id {
            return Err(LspTransportError::InvalidResponse {
                method: method.to_string(),
                details: format!(
                    "mismatched id: expected {:?}, got {:?}",
                    id, response.id
                ),
            });
        }

        if let Some(error) = &response.error {
            return Err(LspTransportError::JsonRpc(error.clone()));
        }

        Ok(response)
    }

    /// Read a single message from the server, returning either a response or
    /// a server-initiated notification (e.g. `publishDiagnostics`).
    pub async fn read_message(&mut self) -> Result<LspServerMessage, LspTransportError> {
        let payload = self.read_frame().await?;
        let value: JsonValue = serde_json::from_slice(&payload).map_err(|error| {
            LspTransportError::InvalidResponse {
                method: "unknown".to_string(),
                details: error.to_string(),
            }
        })?;

        // Responses have an "id" field; notifications have "method" but no "id"
        if value.get("id").is_some() {
            let response: LspResponse = serde_json::from_value(value).map_err(|error| {
                LspTransportError::InvalidResponse {
                    method: "unknown".to_string(),
                    details: format!("failed to parse response: {error}"),
                }
            })?;
            Ok(LspServerMessage::Response(response))
        } else if value.get("method").is_some() {
            let notification: LspNotification = serde_json::from_value(value).map_err(|error| {
                LspTransportError::InvalidResponse {
                    method: "unknown".to_string(),
                    details: format!("failed to parse notification: {error}"),
                }
            })?;
            Ok(LspServerMessage::Notification(notification))
        } else {
            Err(LspTransportError::InvalidResponse {
                method: "unknown".to_string(),
                details: "message has neither 'id' nor 'method'".to_string(),
            })
        }
    }

    /// Read a response from the server. Interleaved notifications are queued.
    pub async fn read_response(&mut self) -> Result<LspResponse, LspTransportError> {
        loop {
            match self.read_message().await? {
                LspServerMessage::Response(r) => return Ok(r),
                LspServerMessage::Notification(n) => {
                    self.pending_notifications.push(n);
                }
            }
        }
    }

    /// Drain and return all queued server-initiated notifications.
    pub fn drain_notifications(&mut self) -> Vec<LspNotification> {
        std::mem::take(&mut self.pending_notifications)
    }

    pub async fn shutdown(&mut self) -> Result<(), LspTransportError> {
        let _ = self
            .send_notification("shutdown", None)
            .await;

        let _ = self.send_notification("exit", None).await;

        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = self.child.kill().await;
            }
        }

        Ok(())
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn write_frame(&mut self, payload: &[u8]) -> Result<(), LspTransportError> {
        let header = format!("Content-Length: {}\r\n\r\n", payload.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(payload).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>, LspTransportError> {
        let mut content_length: Option<usize> = None;

        loop {
            let mut line = String::new();
            let bytes_read = self.stdout.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Err(LspTransportError::ServerExited);
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
                        .map_err(|error| LspTransportError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            error,
                        )))?;
                    content_length = Some(parsed);
                }
            }
        }

        let content_length = content_length.ok_or_else(|| {
            LspTransportError::InvalidResponse {
                method: "unknown".to_string(),
                details: "missing Content-Length header".to_string(),
            }
        })?;

        let mut payload = vec![0u8; content_length];
        self.stdout.read_exact(&mut payload).await.map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                LspTransportError::ServerExited
            } else {
                LspTransportError::Io(error)
            }
        })?;

        Ok(payload)
    }

    /// Connect to an LSP server over TCP (e.g. Godot on localhost:6008).
    /// The command should be a `tcp://host:port` URI.
    /// Uses `socat` or `nc` as a stdio↔TCP bridge so that the same
    /// Content-Length framing logic works unchanged.
    pub fn connect_tcp(address: &str) -> io::Result<Self> {
        Self::connect_tcp_with_timeout(address, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn connect_tcp_with_timeout(
        address: &str,
        request_timeout: Duration,
    ) -> io::Result<Self> {
        let addr = address.trim_start_matches("tcp://");

        // Try socat first (reliable bidirectional bridge)
        let socat_available = std::process::Command::new("socat")
            .arg("-V")
            .output()
            .is_ok();

        let mut cmd = if socat_available {
            let mut c = Command::new("socat");
            c.args([
                "-", // stdin/stdout
                &format!("TCP:{addr}"),
            ]);
            c
        } else {
            // Fall back to nc (netcat)
            let mut c = Command::new("nc");
            // Parse host:port
            let mut parts = addr.split(':');
            let host = parts.next().unwrap_or("localhost");
            let port = parts.next().unwrap_or("6008");
            c.args([host, port]);
            c
        };

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("TCP bridge process missing stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("TCP bridge process missing stdout pipe"))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            request_timeout,
            pending_notifications: Vec::new(),
        })
    }
}




#[cfg(test)]
mod tests;
