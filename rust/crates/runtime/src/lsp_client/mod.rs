#![allow(clippy::should_implement_trait, clippy::must_use_candidate)]
//! LSP (Language Server Protocol) client registry for tool dispatch.

mod dispatch;
mod types;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_lifecycle;

pub use types::{
    LspAction, LspCodeAction, LspCodeLens, LspCommand, LspCompletionItem, LspDiagnostic,
    LspFileEdit, LspHoverResult, LspLocation, LspParameterInfo, LspRenameResult,
    LspServerState, LspServerStatus, LspSignatureHelpResult, LspSignatureInformation,
    LspSymbol, LspTextEdit, LspWorkspaceEdit,
};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::lsp_discovery::{discover_available_servers, LspServerDescriptor};
use crate::lsp_process::LspProcess;

/// Entry in the LSP registry combining process handle, descriptor, and state.
struct LspServerEntry {
    /// The running LSP process, if started. Wrapped in Arc<Mutex<>> for thread-safe async access.
    process: Option<Arc<Mutex<LspProcess>>>,
    /// The server descriptor for lazy-start on first use.
    descriptor: Option<LspServerDescriptor>,
    /// The server state metadata (status, capabilities, diagnostics).
    state: LspServerState,
}

impl std::fmt::Debug for LspServerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspServerEntry")
            .field("process", &self.process.is_some())
            .field("descriptor", &self.descriptor)
            .field("state", &self.state)
            .finish()
    }
}

impl LspServerEntry {
    fn new(state: LspServerState) -> Self {
        Self {
            process: None,
            descriptor: None,
            state,
        }
    }

    fn with_descriptor(state: LspServerState, descriptor: LspServerDescriptor) -> Self {
        Self {
            process: None,
            descriptor: Some(descriptor),
            state,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LspRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    servers: HashMap<String, LspServerEntry>,
    open_files: HashSet<String>,
}

impl LspRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an LSP server with metadata but without starting the process.
    /// The server can be started later via `start_server()` or lazily on first `dispatch()`.
    pub fn register(
        &self,
        language: &str,
        status: LspServerStatus,
        root_path: Option<&str>,
        capabilities: Vec<String>,
    ) {
        let state = LspServerState {
            language: language.to_owned(),
            status,
            root_path: root_path.map(str::to_owned),
            capabilities,
            diagnostics: Vec::new(),
        };
        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner
            .servers
            .insert(language.to_owned(), LspServerEntry::new(state));
    }

    /// Register an LSP server with a descriptor for lazy-start.
    /// The descriptor provides the command and args to start the server when needed.
    pub fn register_with_descriptor(
        &self,
        language: &str,
        status: LspServerStatus,
        root_path: Option<&str>,
        capabilities: Vec<String>,
        descriptor: LspServerDescriptor,
    ) {
        let state = LspServerState {
            language: language.to_owned(),
            status,
            root_path: root_path.map(str::to_owned),
            capabilities,
            diagnostics: Vec::new(),
        };
        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner.servers.insert(
            language.to_owned(),
            LspServerEntry::with_descriptor(state, descriptor),
        );
    }

    pub fn get(&self, language: &str) -> Option<LspServerState> {
        let inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner.servers.get(language).map(|entry| entry.state.clone())
    }

    /// Find the appropriate server for a file path based on extension.
    pub fn find_server_for_path(&self, path: &str) -> Option<LspServerState> {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let language = match ext {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            "c" | "h" => "c",
            "cpp" | "hpp" | "cc" => "cpp",
            "rb" => "ruby",
            "lua" => "lua",
            "html" | "htm" => "html",
            "css" | "scss" | "less" | "sass" => "css",
            "json" | "jsonc" => "json",
            "sh" | "bash" | "zsh" => "bash",
            "yaml" | "yml" => "yaml",
            "gd" => "gdscript",
            _ => return None,
        };

        self.get(language)
    }

    /// Get the language name for a file path based on extension.
    fn language_for_path(path: &str) -> Option<String> {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())?;

        let language = match ext {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            "c" | "h" => "c",
            "cpp" | "hpp" | "cc" => "cpp",
            "rb" => "ruby",
            "lua" => "lua",
            "html" | "htm" => "html",
            "css" | "scss" | "less" | "sass" => "css",
            "json" | "jsonc" => "json",
            "sh" | "bash" | "zsh" => "bash",
            "yaml" | "yml" => "yaml",
            "gd" => "gdscript",
            _ => return None,
        };

        Some(language.to_owned())
    }

    /// List all registered servers.
    pub fn list_servers(&self) -> Vec<LspServerState> {
        let inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner.servers.values().map(|entry| entry.state.clone()).collect()
    }

    /// Add diagnostics to a server.
    pub fn add_diagnostics(
        &self,
        language: &str,
        diagnostics: Vec<LspDiagnostic>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        let entry = inner
            .servers
            .get_mut(language)
            .ok_or_else(|| format!("LSP server not found for language: {language}"))?;
        entry.state.diagnostics.extend(diagnostics);
        Ok(())
    }

    /// Get diagnostics for a specific file path.
    pub fn get_diagnostics(&self, path: &str) -> Vec<LspDiagnostic> {
        let inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner
            .servers
            .values()
            .flat_map(|entry| &entry.state.diagnostics)
            .filter(|d| d.path == path)
            .cloned()
            .collect()
    }

    /// Clear diagnostics for a language server.
    pub fn clear_diagnostics(&self, language: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        let entry = inner
            .servers
            .get_mut(language)
            .ok_or_else(|| format!("LSP server not found for language: {language}"))?;
        entry.state.diagnostics.clear();
        Ok(())
    }

    /// Disconnect a server.
    pub fn disconnect(&self, language: &str) -> Option<LspServerState> {
        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner.servers.remove(language).map(|entry| entry.state)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("lsp registry lock poisoned");
        inner.servers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Start an LSP server process for the given language.
    /// If the process is already running, this is a no-op.
    /// If a descriptor is available, it is used to start the process.
    /// If no descriptor is available, the discovery system is consulted.
    pub fn start_server(&self, language: &str) -> Result<(), String> {
        // Check if already running
        {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get(language) {
                if entry.process.is_some() {
                    return Ok(());
                }
            }
        }

        // Try to get the descriptor
        let descriptor = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get(language) {
                entry.descriptor.clone()
            } else {
                None
            }
        };

        // If no descriptor, try discovery
        let descriptor = if let Some(d) = descriptor { d } else {
            let available = discover_available_servers();
            available
                .into_iter()
                .find(|d| d.language == language)
                .ok_or_else(|| {
                    format!("no LSP server descriptor found for language: {language}")
                })?
        };

        let root_path = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner
                .servers
                .get(language)
                .and_then(|entry| entry.state.root_path.clone())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .map_or_else(|_| ".".to_owned(), |p| p.to_string_lossy().into_owned())
                })
        };

        let process = {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
            rt.block_on(LspProcess::start(
                &descriptor.command,
                &descriptor.args,
                Path::new(&root_path),
            ))
            .map_err(|e| format!("failed to start LSP server for '{language}': {e}"))?
        };

        let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
        if let Some(entry) = inner.servers.get_mut(language) {
            entry.process = Some(Arc::new(Mutex::new(process)));
            entry.state.status = LspServerStatus::Connected;
        }

        Ok(())
    }

    /// Stop a running LSP server process.
    pub fn stop_server(&self, language: &str) -> Result<(), String> {
        let process_arc = {
            let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
            let entry = inner
                .servers
                .get_mut(language)
                .ok_or_else(|| format!("LSP server not found for language: {language}"))?;
            entry.state.status = LspServerStatus::Disconnected;
            entry.process.take()
        };

        if let Some(process_arc) = process_arc {
            let mut process = process_arc
                .lock()
                .map_err(|_| "lsp process lock poisoned")?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
            rt.block_on(process.shutdown())
                .map_err(|e| format!("LSP shutdown error: {e}"))?;
        }

        Ok(())
    }

    /// Notify the LSP server that a file was opened and collect any diagnostics.
    /// Best-effort: returns empty vec if no server is available.
    pub fn notify_file_open(&self, path: &str, content: &str) -> Vec<LspDiagnostic> {
        let Some(language) = Self::language_for_path(path) else {
            return Vec::new();
        };

        // Check if already open
        {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            if inner.open_files.contains(path) {
                return Vec::new();
            }
        }

        // Lazy-start the server
        if self.start_server(&language).is_err() {
            return Vec::new();
        }

        // Get the process handle and send didOpen
        let process_arc = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            match inner.servers.get(&language).and_then(|e| e.process.clone()) {
                Some(p) => p,
                None => return Vec::new(),
            }
        };

        let mut diagnostics = Vec::new();
        if let Ok(mut process) = process_arc.lock() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                let _ = rt.block_on(process.did_open(path, content));
                diagnostics = process.drain_diagnostics();
            }
        }

        // Cache diagnostics in registry state
        if !diagnostics.is_empty() {
            let diag_path = path.to_owned();
            let diags = diagnostics.clone();
            let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get_mut(&language) {
                // Replace diagnostics for this file (publishDiagnostics is full replacement)
                entry.state.diagnostics.retain(|d| d.path != diag_path);
                entry.state.diagnostics.extend(diags);
            }
        }

        // Mark file as open
        {
            let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner.open_files.insert(path.to_owned());
        }

        diagnostics
    }

    /// Notify the LSP server that a file changed and collect any diagnostics.
    /// Best-effort: returns empty vec if no server is available.
    pub fn notify_file_change(&self, path: &str, content: &str) -> Vec<LspDiagnostic> {
        let Some(language) = Self::language_for_path(path) else {
            return Vec::new();
        };

        // Get the process handle
        let process_arc = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            match inner.servers.get(&language).and_then(|e| e.process.clone()) {
                Some(p) => p,
                None => return Vec::new(),
            }
        };

        let mut diagnostics = Vec::new();
        if let Ok(mut process) = process_arc.lock() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                let _ = rt.block_on(process.did_change(path, content));
                diagnostics = process.drain_diagnostics();
            }
        }

        // Replace cached diagnostics for this file
        if !diagnostics.is_empty() {
            let diag_path = path.to_owned();
            let diags = diagnostics.clone();
            let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
            if let Some(entry) = inner.servers.get_mut(&language) {
                entry.state.diagnostics.retain(|d| d.path != diag_path);
                entry.state.diagnostics.extend(diags);
            }
        }

        diagnostics
    }


    /// Notify the LSP server that a file was closed.
    /// Best-effort: returns empty vec if no server is available.
    pub fn notify_file_close(&self, path: &str) -> Vec<LspDiagnostic> {
        let Some(language) = Self::language_for_path(path) else {
            return Vec::new();
        };

        let process_arc = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            match inner.servers.get(&language).and_then(|e| e.process.clone()) {
                Some(p) => p,
                None => return Vec::new(),
            }
        };

        if let Ok(mut process) = process_arc.lock() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                let _ = rt.block_on(process.did_close(path));
            }
        }

        // Mark file as closed
        {
            let mut inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner.open_files.remove(path);
        }

        Vec::new()
    }
    /// Fetch diagnostics for a file by draining pending server notifications
    /// and returning cached diagnostics.
    pub fn fetch_diagnostics_for_file(&self, path: &str) -> Vec<LspDiagnostic> {
        let Some(language) = Self::language_for_path(path) else {
            return Vec::new();
        };

        // Drain pending notifications from the transport
        let process_arc = {
            let inner = self.inner.lock().expect("lsp registry lock poisoned");
            inner.servers.get(&language).and_then(|e| e.process.clone())
        };

        if let Some(process_arc) = process_arc {
            if let Ok(mut process) = process_arc.lock() {
                let new_diags = process.drain_diagnostics();
                if !new_diags.is_empty() {
                    let diag_path = path.to_owned();
                    let mut inner =
                        self.inner.lock().expect("lsp registry lock poisoned");
                    if let Some(entry) = inner.servers.get_mut(&language) {
                        entry.state.diagnostics.retain(|d| d.path != diag_path);
                        entry.state.diagnostics.extend(new_diags);
                    }
                }
            }
        }

        self.get_diagnostics(path)
    }
}
