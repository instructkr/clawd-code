//! LSP type definitions: action enums, diagnostic/location types, server status,
//! and structured results for all supported LSP features.

use serde::{Deserialize, Serialize};

/// Supported LSP actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspAction {
    Diagnostics,
    Hover,
    Definition,
    References,
    Completion,
    Symbols,
    Format,
    CodeAction,
    Rename,
    SignatureHelp,
    CodeLens,
    WorkspaceSymbols,
}

impl LspAction {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "diagnostics" => Some(Self::Diagnostics),
            "hover" => Some(Self::Hover),
            "definition" | "goto_definition" => Some(Self::Definition),
            "references" | "find_references" => Some(Self::References),
            "completion" | "completions" => Some(Self::Completion),
            "symbols" | "document_symbols" => Some(Self::Symbols),
            "format" | "formatting" => Some(Self::Format),
            "code_action" | "codeaction" => Some(Self::CodeAction),
            "rename" => Some(Self::Rename),
            "signature_help" | "signatures" => Some(Self::SignatureHelp),
            "code_lens" | "codelens" => Some(Self::CodeLens),
            "workspace_symbols" => Some(Self::WorkspaceSymbols),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub path: String,
    pub line: u32,
    pub character: u32,
    pub severity: String,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspLocation {
    pub path: String,
    pub line: u32,
    pub character: u32,
    pub end_line: Option<u32>,
    pub end_character: Option<u32>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspHoverResult {
    pub content: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspCompletionItem {
    pub label: String,
    pub kind: Option<String>,
    pub detail: Option<String>,
    pub insert_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
    pub character: u32,
}

/// A code action (quick fix, refactor, etc.) returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub edit: Option<LspWorkspaceEdit>,
    pub command: Option<LspCommand>,
}

/// A workspace edit containing multiple file changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspWorkspaceEdit {
    pub changes: Vec<LspFileEdit>,
}

/// Edits to a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspFileEdit {
    pub path: String,
    pub edits: Vec<LspTextEdit>,
}

/// A single text edit operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspTextEdit {
    pub new_text: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A command that the server requests the client to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspCommand {
    pub title: String,
    pub command: String,
    pub arguments: Vec<serde_json::Value>,
}

/// Result of a rename operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspRenameResult {
    pub new_name: String,
    pub edit: Option<LspWorkspaceEdit>,
}

/// A single parameter in a function signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspParameterInfo {
    pub label: String,
    pub documentation: Option<String>,
}

/// A function signature with its parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspSignatureInformation {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<LspParameterInfo>,
    pub active_parameter: Option<u32>,
}

/// Result of a signature help request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspSignatureHelpResult {
    pub signatures: Vec<LspSignatureInformation>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

/// A code lens item — an actionable hint inline in the editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspCodeLens {
    pub line: u32,
    pub character: u32,
    pub command: Option<LspCommand>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspServerStatus {
    Connected,
    Disconnected,
    Starting,
    Error,
}

impl std::fmt::Display for LspServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected => write!(f, "connected"),
            Self::Disconnected => write!(f, "disconnected"),
            Self::Starting => write!(f, "starting"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerState {
    pub language: String,
    pub status: LspServerStatus,
    pub root_path: Option<String>,
    pub capabilities: Vec<String>,
    pub diagnostics: Vec<LspDiagnostic>,
}
