//! LSP action dispatch: routes actions to the appropriate server process.

use super::types::{LspAction, LspServerStatus};
use crate::lsp_process::LspProcessError;

impl super::LspRegistry {
    /// Dispatch an LSP action and return a structured result.
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(
        &self,
        action: &str,
        path: Option<&str>,
        line: Option<u32>,
        character: Option<u32>,
        _query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let lsp_action =
            LspAction::from_str(action).ok_or_else(|| format!("unknown LSP action: {action}"))?;

        // For diagnostics, we check existing cached diagnostics
        if lsp_action == LspAction::Diagnostics {
            if let Some(path) = path {
                let diags = self.get_diagnostics(path);
                return Ok(serde_json::json!({
                    "action": "diagnostics",
                    "path": path,
                    "diagnostics": diags,
                    "count": diags.len()
                }));
            }
            // All diagnostics across all servers
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            let all_diags: Vec<_> = inner
                .servers
                .values()
                .flat_map(|entry| &entry.state.diagnostics)
                .collect();
            return Ok(serde_json::json!({
                "action": "diagnostics",
                "diagnostics": all_diags,
                "count": all_diags.len()
            }));
        }

        // For other actions, we need a connected server for the given file
        // (workspace_symbols operates without a specific file path)
        let language = if lsp_action == LspAction::WorkspaceSymbols {
            // Try to find any connected server for workspace symbols
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner.servers.keys().next().cloned()
                .ok_or_else(|| "no LSP servers available for workspace symbols".to_owned())?
        } else {
            let p = path.ok_or("path is required for this LSP action")?;
            Self::language_for_path(p)
                .ok_or_else(|| format!("no LSP server available for path: {p}"))?
        };
        let path = path.unwrap_or("");

        // Check the entry exists
        {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if !inner.servers.contains_key(&language) {
                return Err(format!("no LSP server available for path: {path}"));
            }
        }

        // Check if the server is already in a non-starting state
        {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get(&language) {
                if entry.state.status == LspServerStatus::Disconnected
                    || entry.state.status == LspServerStatus::Error
                {
                    if entry.process.is_none() {
                        return Err(format!(
                            "LSP server for '{}' is not connected (status: {})",
                            language, entry.state.status
                        ));
                    }
                }
            }
        }

        // Lazy-start: if no process yet, try to start one
        let needs_start = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner
                .servers
                .get(&language)
                .is_none_or(|entry| entry.process.is_none())
        };

        if needs_start {
            if let Err(e) = self.start_server(&language) {
                // Check the status after failed start — if still not Connected,
                // return a proper error. This preserves the existing behavior
                // for Disconnected/Error status servers.
                let inner = self.inner.lock().expect("lsp registry lock poisoned");
                if let Some(entry) = inner.servers.get(&language) {
                    if entry.state.status != LspServerStatus::Connected {
                        return Err(format!(
                            "LSP server for '{}' is not connected (status: {}): {}",
                            language, entry.state.status, e
                        ));
                    }
                }
                // If somehow still marked Connected but start failed, return error JSON
                return Ok(serde_json::json!({
                    "action": action,
                    "path": path,
                    "line": line,
                    "character": character,
                    "language": language,
                    "status": "error",
                    "error": e
                }));
            }
        }

        // Check the server status
        {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get(&language) {
                if entry.state.status != LspServerStatus::Connected {
                    return Err(format!(
                        "LSP server for '{}' is not connected (status: {})",
                        language, entry.state.status
                    ));
                }
            }
        }

        // Get the process handle (clone the Arc)
        let process_arc = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner
                .servers
                .get(&language)
                .and_then(|entry| entry.process.clone())
                .ok_or_else(|| format!("no LSP process available for language: {language}"))?
        };

        // Dispatch to the real LSP process
        let result = {
            let mut process = process_arc
                .lock()
                .map_err(|_| "lsp process lock poisoned".to_owned())?;

            // Create a minimal tokio runtime for async LSP calls
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to create tokio runtime: {e}"))?;

            rt.block_on(async {
                let line = line.unwrap_or(0);
                let character = character.unwrap_or(0);

                match lsp_action {
                    LspAction::Hover => {
                        let hover = process.hover(path, line, character).await;
                        hover.map(|opt| {
                            opt.map_or_else(
                                || serde_json::json!({
                                    "action": "hover",
                                    "path": path,
                                    "line": line,
                                    "character": character,
                                    "language": language,
                                    "status": "no_result",
                                }),
                                |h| serde_json::json!({
                                    "action": "hover",
                                    "path": path,
                                    "line": line,
                                    "character": character,
                                    "language": language,
                                    "status": "ok",
                                    "result": h,
                                }),
                            )
                        })
                    }
                    LspAction::Definition => {
                        let locations = process.goto_definition(path, line, character).await;
                        locations.map(|locs| serde_json::json!({
                            "action": "definition",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "locations": locs,
                        }))
                    }
                    LspAction::References => {
                        let locations = process.references(path, line, character).await;
                        locations.map(|locs| serde_json::json!({
                            "action": "references",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "locations": locs,
                        }))
                    }
                    LspAction::Completion => {
                        let items = process.completion(path, line, character).await;
                        items.map(|completions| serde_json::json!({
                            "action": "completion",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "items": completions,
                        }))
                    }
                    LspAction::Symbols => {
                        let symbols = process.document_symbols(path).await;
                        symbols.map(|syms| serde_json::json!({
                            "action": "symbols",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "symbols": syms,
                        }))
                    }
                    LspAction::Format => {
                        let edits = process.format(path).await;
                        edits.map(|text_edits| serde_json::json!({
                            "action": "format",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "edits": text_edits,
                        }))
                    }
                    LspAction::CodeAction => {
                        let end_line = if line > 0 { Some(line) } else { None };
                        let end_character = if character > 0 { Some(character) } else { None };
                        let actions = process.code_action(path, line, character, end_line, end_character, None).await;
                        actions.map(|acts| serde_json::json!({
                            "action": "code_action",
                            "path": path,
                            "line": 0,
                            "character": 0,
                            "end_line": end_line,
                            "end_character": end_character,
                            "language": language,
                            "status": "ok",
                            "actions": acts,
                        }))
                    }
                    LspAction::Rename => {
                        let new_name = _query.ok_or_else(|| LspProcessError::InvalidRequest("new_name required for rename".into()))?;
                        let rename_result = process.rename(path, line, character, new_name).await;
                        rename_result.map(|r| serde_json::json!({
                            "action": "rename",
                            "path": path,
                            "line": line,
                            "character": character,
                            "language": language,
                            "status": "ok",
                            "result": r,
                        }))
                    }
                    LspAction::SignatureHelp => {
                        let sig = process.signature_help(path, line, character).await;
                        sig.map(|opt| {
                            opt.map_or_else(
                                || serde_json::json!({
                                    "action": "signature_help",
                                    "path": path,
                                    "line": line,
                                    "character": character,
                                    "language": language,
                                    "status": "no_result",
                                }),
                                |s| serde_json::json!({
                                    "action": "signature_help",
                                    "path": path,
                                    "line": line,
                                    "character": character,
                                    "language": language,
                                    "status": "ok",
                                    "result": s,
                                }),
                            )
                        })
                    }
                    LspAction::CodeLens => {
                        let lenses = process.code_lens(path).await;
                        lenses.map(|l| serde_json::json!({
                            "action": "code_lens",
                            "path": path,
                            "language": language,
                            "status": "ok",
                            "lenses": l,
                        }))
                    }
                    LspAction::WorkspaceSymbols => {
                        let query = _query.unwrap_or("");
                        let symbols = process.workspace_symbols(query).await;
                        symbols.map(|syms| serde_json::json!({
                            "action": "workspace_symbols",
                            "language": language,
                            "query": query,
                            "status": "ok",
                            "symbols": syms,
                        }))
                    }
                    LspAction::Diagnostics => unreachable!(),
                }
            })
        };

        result.map_err(|e| format!("LSP {action} failed for '{language}': {e}"))
    }
}
