//! LSP process manager: spawns language servers and drives the LSP lifecycle.

mod parse;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value as JsonValue;

use crate::lsp_client::{
    LspCodeAction, LspCodeLens, LspCompletionItem, LspDiagnostic, LspHoverResult, LspLocation,
    LspRenameResult, LspServerStatus, LspSignatureHelpResult, LspSymbol,
};
use crate::lsp_transport::{LspTransport, LspTransportError};

use parse::{
    canonicalize_root, language_id_for_path, parse_code_actions, parse_code_lens,
    parse_completions, parse_hover, parse_locations, parse_signature_help,
    parse_symbols, parse_workspace_edit, parse_workspace_symbols, path_to_uri,
    rename_params, severity_name, text_document_position_params, uri_to_path,
    workspace_symbol_params,
};

#[derive(Debug)]
pub struct LspProcess {
    transport: LspTransport,
    language: String,
    root_uri: String,
    capabilities: Option<JsonValue>,
    status: LspServerStatus,
    open_files: HashSet<String>,
    version_counter: HashMap<String, u32>,
}

#[allow(clippy::cast_possible_truncation)]
impl LspProcess {
    /// Spawn a language server process and perform the LSP initialize handshake.
    pub async fn start(
        command: &str,
        args: &[String],
        root_path: &Path,
    ) -> Result<Self, LspProcessError> {
        let transport = if command.starts_with("tcp://") {
            LspTransport::connect_tcp(command)
                .map_err(|e| LspProcessError::Transport(LspTransportError::Io(e)))?
        } else {
            LspTransport::spawn(command, args)
                .map_err(|e| LspProcessError::Transport(LspTransportError::Io(e)))?
        };

        let canonical = canonicalize_root(root_path)?;
        let root_uri = format!("file://{canonical}");

        let mut process = Self {
            transport,
            language: command.to_owned(),
            root_uri: root_uri.clone(),
            capabilities: None,
            status: LspServerStatus::Starting,
            open_files: HashSet::new(),
            version_counter: HashMap::new(),
        };

        process.initialize(&canonical).await?;
        process.status = LspServerStatus::Connected;

        Ok(process)
    }

    /// Send the LSP `initialize` request followed by the `initialized` notification.
    async fn initialize(&mut self, root_path: &str) -> Result<JsonValue, LspProcessError> {
        let root_uri = format!("file://{root_path}");
        let pid = std::process::id();

        let params = serde_json::json!({
            "processId": pid,
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": "root" }],
            "capabilities": {
                "textDocument": {
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "completion": {
                        "completionItem": { "snippetSupport": false }
                    },
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": { "relatedInformation": true },
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": [
                                    "", "quickfix", "refactor", "refactor.extract",
                                    "refactor.inline", "refactor.rewrite", "source",
                                    "source.organizeImports"
                                ]
                            }
                        }
                    },
                    "rename": { "prepareSupport": true },
                    "signatureHelp": {
                        "signatureInformation": {
                            "documentationFormat": ["markdown", "plaintext"],
                            "parameterInformation": { "labelOffsetSupport": true }
                        }
                    },
                    "codeLens": {}
                },
                "workspace": {
                    "symbol": {},
                    "workspaceFolders": true
                }
            }
        });

        let response = self
            .transport
            .send_request("initialize", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        self.capabilities = Some(result.clone());

        self.transport
            .send_notification("initialized", Some(serde_json::json!({})))
            .await
            .map_err(LspProcessError::Transport)?;

        Ok(result)
    }

    /// Gracefully shut down the language server.
    pub async fn shutdown(&mut self) -> Result<(), LspProcessError> {
        self.status = LspServerStatus::Disconnected;

        let shutdown_result = self
            .transport
            .send_request("shutdown", None)
            .await
            .map_err(LspProcessError::Transport);

        if shutdown_result.is_ok() {
            self.transport
                .send_notification("exit", None)
                .await
                .map_err(LspProcessError::Transport)?;
        }

        self.transport
            .shutdown()
            .await
            .map_err(LspProcessError::Transport)?;

        Ok(())
    }

    /// Query hover information at a position.
    pub async fn hover(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<LspHoverResult>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = text_document_position_params(&uri, line, character);

        let response = self
            .transport
            .send_request("textDocument/hover", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        if result.is_null() {
            return Ok(None);
        }

        Ok(parse_hover(&result))
    }

    /// Go to definition at a position.
    pub async fn goto_definition(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspLocation>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = text_document_position_params(&uri, line, character);

        let response = self
            .transport
            .send_request("textDocument/definition", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        Ok(parse_locations(&result))
    }

    /// Find references at a position.
    pub async fn references(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspLocation>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        });

        let response = self
            .transport
            .send_request("textDocument/references", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        Ok(parse_locations(&result))
    }

    /// Get document symbols for a file.
    pub async fn document_symbols(
        &mut self,
        path: &str,
    ) -> Result<Vec<LspSymbol>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });

        let response = self
            .transport
            .send_request("textDocument/documentSymbol", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        Ok(parse_symbols(&result, path))
    }

    /// Get completions at a position.
    pub async fn completion(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCompletionItem>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = text_document_position_params(&uri, line, character);

        let response = self
            .transport
            .send_request("textDocument/completion", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        // The response may be a CompletionList or a plain array.
        let items = if let Some(list) = result.get("items") {
            list
        } else {
            &result
        };

        Ok(parse_completions(items))
    }

    /// Format a document.
    pub async fn format(&mut self, path: &str) -> Result<Vec<JsonValue>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        });

        let response = self
            .transport
            .send_request("textDocument/formatting", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        match result.as_array() {
            Some(arr) => Ok(arr.clone()),
            None => Ok(Vec::new()),
        }
    }

    /// Notify the server that a file was opened. Sends `textDocument/didOpen`.
    /// No-op if the file is already tracked as open.
    pub async fn did_open(&mut self, path: &str, content: &str) -> Result<(), LspProcessError> {
        if self.open_files.contains(path) {
            return Ok(());
        }

        let uri = path_to_uri(path);
        let language_id = language_id_for_path(path);
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 0,
                "text": content
            }
        });

        self.transport
            .send_notification("textDocument/didOpen", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        self.open_files.insert(path.to_owned());
        self.version_counter.insert(path.to_owned(), 0);
        Ok(())
    }

    /// Notify the server that a file's content changed. Sends `textDocument/didChange`.
    pub async fn did_change(&mut self, path: &str, content: &str) -> Result<(), LspProcessError> {
        let version = self.version_counter.get(path).map_or(1, |v| v + 1);

        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": content }]
        });

        self.transport
            .send_notification("textDocument/didChange", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;

        self.version_counter.insert(path.to_owned(), version);
        Ok(())
    }


    /// Notify the server that a file was closed. Sends `textDocument/didClose`.
    pub async fn did_close(&mut self, path: &str) -> Result<(), LspProcessError> {
        if !self.open_files.contains(path) {
            return Ok(());
        }
        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });
        self.transport
            .send_notification("textDocument/didClose", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        self.open_files.remove(path);
        self.version_counter.remove(path);
        Ok(())
    }

    /// Request code actions (quick fixes, refactors) for a range in a file.
    pub async fn code_action(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
        end_line: Option<u32>,
        end_character: Option<u32>,
        only_kinds: Option<&[String]>,
    ) -> Result<Vec<LspCodeAction>, LspProcessError> {
        let uri = path_to_uri(path);
        let el = end_line.unwrap_or(line);
        let ec = end_character.unwrap_or(character);
        let mut params = serde_json::json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": el, "character": ec }
            },
            "context": { "diagnostics": [] }
        });
        if let Some(kinds) = only_kinds {
            params["context"]["only"] = serde_json::json!(kinds);
        }
        let response = self
            .transport
            .send_request("textDocument/codeAction", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;
        Ok(parse_code_actions(&result))
    }

    /// Rename a symbol at a position across the workspace.
    pub async fn rename(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<LspRenameResult, LspProcessError> {
        let uri = path_to_uri(path);
        let params = rename_params(&uri, line, character, new_name);
        let response = self
            .transport
            .send_request("textDocument/rename", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;
        let edit = parse_workspace_edit(&result);
        Ok(LspRenameResult {
            new_name: new_name.to_owned(),
            edit,
        })
    }

    /// Get signature help at a position (function signatures, parameters).
    pub async fn signature_help(
        &mut self,
        path: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<LspSignatureHelpResult>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = text_document_position_params(&uri, line, character);
        let response = self
            .transport
            .send_request("textDocument/signatureHelp", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;
        if result.is_null() {
            return Ok(None);
        }
        Ok(parse_signature_help(&result))
    }

    /// Get code lens items for a file (actionable inline hints).
    pub async fn code_lens(&mut self, path: &str) -> Result<Vec<LspCodeLens>, LspProcessError> {
        let uri = path_to_uri(path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });
        let response = self
            .transport
            .send_request("textDocument/codeLens", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        Ok(parse_code_lens(&result))
    }

    /// Search for symbols across the entire workspace.
    pub async fn workspace_symbols(
        &mut self,
        query: &str,
    ) -> Result<Vec<LspSymbol>, LspProcessError> {
        let params = workspace_symbol_params(query);
        let response = self
            .transport
            .send_request("workspace/symbol", Some(params))
            .await
            .map_err(LspProcessError::Transport)?;
        let result = response
            .into_result()
            .map_err(|e| LspProcessError::Transport(LspTransportError::JsonRpc(e)))?;
        Ok(parse_workspace_symbols(&result))
    }

    /// Drain queued server notifications and extract `publishDiagnostics`.
    #[allow(clippy::redundant_closure_for_method_calls)]
    pub fn drain_diagnostics(&mut self) -> Vec<LspDiagnostic> {
        let notifications = self.transport.drain_notifications();
        let mut diagnostics = Vec::new();
        for n in &notifications {
            if n.method == "textDocument/publishDiagnostics" {
                if let Some(params) = &n.params {
                    if let Some(uri) = params.get("uri").and_then(|v| v.as_str()) {
                        let path = uri_to_path(uri);
                        if let Some(diags) = params.get("diagnostics").and_then(|v| v.as_array())
                        {
                            for d in diags {
                                diagnostics.push(LspDiagnostic {
                                    path: path.clone(),
                                    line: d
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("line"))
                                        .and_then(|v| v.as_u64())
                                        .map_or(0, |v| v as u32),
                                    character: d
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("character"))
                                        .and_then(|v| v.as_u64())
                                        .map_or(0, |v| v as u32),
                                    severity: d
                                        .get("severity")
                                        .and_then(|v| v.as_u64())
                                        .map_or_else(|| "error".to_owned(), severity_name),
                                    message: d
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_owned(),
                                    source: d
                                        .get("source")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_owned),
                                });
                            }
                        }
                    }
                }
            }
        }
        diagnostics
    }

    #[must_use]
    pub fn status(&self) -> LspServerStatus {
        self.status
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    #[must_use]
    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LspProcessError {
    Transport(LspTransportError),
    InvalidPath(String),
    InvalidRequest(String),
}

impl std::fmt::Display for LspProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "LSP transport error: {e}"),
            Self::InvalidPath(p) => write!(f, "invalid path: {p}"),
            Self::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
        }
    }
}

impl std::error::Error for LspProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            Self::InvalidPath(_) | Self::InvalidRequest(_) => None,
        }
    }
}
