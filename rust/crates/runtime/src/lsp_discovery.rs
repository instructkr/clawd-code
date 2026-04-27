//! Auto-discovery of installed LSP servers and file-extension mapping.

use std::path::Path;
use std::process::Command;

/// Descriptor for a well-known LSP server, including its launch command and
/// the file extensions it handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerDescriptor {
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
}

/// Static descriptor used by the [`KNOWN_LSP_SERVERS`] constant. Uses
/// `&'static str` fields so the table can live in read-only memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaticLspServerDescriptor {
    language: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    extensions: &'static [&'static str],
}

impl StaticLspServerDescriptor {
    #[allow(clippy::wrong_self_convention)]
    fn to_descriptor(&self) -> LspServerDescriptor {
        LspServerDescriptor {
            language: self.language.to_string(),
            command: self.command.to_string(),
            args: self.args.iter().map(|s| (*s).to_string()).collect(),
            extensions: self.extensions.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

/// Known LSP servers with their default commands, args, and file extensions.
const KNOWN_LSP_SERVERS_TABLE: &[StaticLspServerDescriptor] = &[
    StaticLspServerDescriptor {
        language: "rust",
        command: "rust-analyzer",
        args: &[],
        extensions: &["rs"],
    },
    StaticLspServerDescriptor {
        language: "c/cpp",
        command: "clangd",
        args: &[],
        extensions: &["c", "h", "cpp", "hpp"],
    },
    StaticLspServerDescriptor {
        language: "python",
        command: "pyright-langserver",
        args: &["--stdio"],
        extensions: &["py"],
    },
    StaticLspServerDescriptor {
        language: "go",
        command: "gopls",
        args: &[],
        extensions: &["go"],
    },
    StaticLspServerDescriptor {
        language: "typescript",
        command: "typescript-language-server",
        args: &["--stdio"],
        extensions: &["ts", "tsx", "js", "jsx"],
    },
    StaticLspServerDescriptor {
        language: "java",
        command: "jdtls",
        args: &[],
        extensions: &["java"],
    },
    StaticLspServerDescriptor {
        language: "ruby",
        command: "solargraph",
        args: &["stdio"],
        extensions: &["rb"],
    },
    StaticLspServerDescriptor {
        language: "lua",
        command: "lua-language-server",
        args: &[],
        extensions: &["lua"],
    },
];

/// Owned copy of the known LSP server descriptors, useful when callers need
/// to mutate or transfer ownership.
#[must_use]
pub fn known_lsp_servers() -> Vec<LspServerDescriptor> {
    KNOWN_LSP_SERVERS_TABLE
        .iter()
        .map(StaticLspServerDescriptor::to_descriptor)
        .collect()
}

/// Check whether a command exists on the user's PATH by attempting to run it
/// with `--version`. Returns `true` if the command could be spawned
/// successfully, `false` otherwise.
///
/// Some LSP servers (like rust-analyzer via rustup) exit non-zero on --version
/// but are still functional. We treat "spawned successfully" as found, regardless
/// of the exit code. Only a failure to spawn (command not found) returns false.
#[must_use]
pub fn command_exists_on_path(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .map_or(false, |_| true)
}

/// Discover LSP servers that are actually installed on the current system.
///
/// Iterates over the known server table and returns only those whose command
/// is found on `PATH`.
#[must_use]
pub fn discover_available_servers() -> Vec<LspServerDescriptor> {
    KNOWN_LSP_SERVERS_TABLE
        .iter()
        .filter(|desc| command_exists_on_path(desc.command))
        .map(StaticLspServerDescriptor::to_descriptor)
        .collect()
}

/// Find the best-matching LSP server descriptor for a given file path.
///
/// Matches on the file extension. If multiple servers share the same
/// extension, the first match wins.
#[must_use]
pub fn find_server_for_file<'a>(
    path: &Path,
    servers: &'a [LspServerDescriptor],
) -> Option<&'a LspServerDescriptor> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    servers
        .iter()
        .find(|desc| desc.extensions.iter().any(|e| e == ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn known_servers_contains_expected_languages() {
        let languages: Vec<&str> = KNOWN_LSP_SERVERS_TABLE
            .iter()
            .map(|s| s.language)
            .collect();
        assert!(languages.contains(&"rust"));
        assert!(languages.contains(&"c/cpp"));
        assert!(languages.contains(&"python"));
        assert!(languages.contains(&"go"));
        assert!(languages.contains(&"typescript"));
        assert!(languages.contains(&"java"));
        assert!(languages.contains(&"ruby"));
        assert!(languages.contains(&"lua"));
    }

    #[test]
    fn find_server_for_rust_file() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(PathBuf::from("src/main.rs").as_path(), &servers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().language, "rust");
    }

    #[test]
    fn find_server_for_python_file() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(PathBuf::from("app.py").as_path(), &servers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().language, "python");
    }

    #[test]
    fn find_server_for_typescript_file() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(PathBuf::from("index.tsx").as_path(), &servers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().language, "typescript");
    }

    #[test]
    fn find_server_for_unknown_extension_returns_none() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(PathBuf::from("data.xyz").as_path(), &servers);
        assert!(result.is_none());
    }

    #[test]
    fn find_server_for_file_without_extension_returns_none() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(PathBuf::from("Makefile").as_path(), &servers);
        assert!(result.is_none());
    }

    #[test]
    fn discover_returns_only_installed_servers() {
        let available = discover_available_servers();
        // Every returned server must have a command that actually exists on PATH.
        for server in &available {
            assert!(
                command_exists_on_path(&server.command),
                "discover_available_servers returned '{}' but command '{}' is not on PATH",
                server.language,
                server.command,
            );
        }
        // If rust-analyzer or clangd are on this system, they should appear.
        let languages: Vec<&str> = available.iter().map(|s| s.language.as_str()).collect();
        if command_exists_on_path("rust-analyzer") {
            assert!(languages.contains(&"rust"), "rust-analyzer is on PATH but 'rust' not in discovered servers");
        }
        if command_exists_on_path("clangd") {
            assert!(languages.contains(&"c/cpp"), "clangd is on PATH but 'c/cpp' not in discovered servers");
        }
    }

    #[test]
    fn find_server_for_rs_file() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(Path::new("src/main.rs"), &servers);
        assert!(result.is_some());
        assert_eq!(result.unwrap().language, "rust");
    }

    #[test]
    fn find_server_for_unknown_extension() {
        let servers = known_lsp_servers();
        let result = find_server_for_file(Path::new("README.md"), &servers);
        assert!(result.is_none());
    }

    #[test]
    fn descriptor_has_correct_args() {
        let servers = known_lsp_servers();
        let rust = servers.iter().find(|s| s.language == "rust").expect("rust server should exist");
        assert!(rust.args.is_empty(), "rust-analyzer should have no args");

        let ts = servers.iter().find(|s| s.language == "typescript").expect("typescript server should exist");
        assert_eq!(ts.args, vec!["--stdio"], "typescript-language-server should have --stdio arg");
    }
}
