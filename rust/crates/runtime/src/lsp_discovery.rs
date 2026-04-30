//! Auto-discovery of installed LSP servers, file-extension mapping, and
//! distro-aware install prompting.

use std::path::Path;
use std::process::Command;

/// Descriptor for a well-known LSP server, including its launch command,
/// the file extensions it handles, and how to install it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerDescriptor {
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
    pub install_hint: Vec<InstallInstruction>,
}

/// A single install command for a specific package manager or platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInstruction {
    pub label: String,
    pub command: String,
}

/// What the caller should do when a server is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspInstallAction {
    /// The server is already available.
    Installed,
    /// The server is not found; these are the suggested install commands.
    Missing { language: String, instructions: Vec<InstallInstruction> },
    /// The server binary exists but is a rustup proxy stub for an uninstalled component.
    RustupProxyMissing { language: String, component: String },
}

/// Detect the current Linux distribution (or non-Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxDistro {
    Debian,
    Ubuntu,
    Fedora,
    Arch,
    OpenSuse,
    Alpine,
    Void,
    NixOS,
    UnknownLinux,
    MacOS,
    Windows,
    Other,
}

/// Static descriptor used by the [`KNOWN_LSP_SERVERS_TABLE`] constant.
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
            install_hint: install_instructions_for(self.language),
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
    StaticLspServerDescriptor {
        language: "html",
        command: "vscode-html-language-server",
        args: &["--stdio"],
        extensions: &["html", "htm"],
    },
    StaticLspServerDescriptor {
        language: "css",
        command: "vscode-css-language-server",
        args: &["--stdio"],
        extensions: &["css", "scss", "less", "sass"],
    },
    StaticLspServerDescriptor {
        language: "json",
        command: "vscode-json-language-server",
        args: &["--stdio"],
        extensions: &["json", "jsonc"],
    },
    StaticLspServerDescriptor {
        language: "bash",
        command: "bash-language-server",
        args: &["start"],
        extensions: &["sh", "bash", "zsh"],
    },
    StaticLspServerDescriptor {
        language: "yaml",
        command: "yaml-language-server",
        args: &["--stdio"],
        extensions: &["yaml", "yml"],
    },
    StaticLspServerDescriptor {
        language: "gdscript",
        command: "tcp://localhost:6008",
        args: &[],
        extensions: &["gd"],
    },
];

/// Return install instructions for a known language server, covering all
/// common distros and package managers. Order doesn't matter — the caller
/// picks the one matching the current system.
fn install_instructions_for(language: &str) -> Vec<InstallInstruction> {
    match language {
        "rust" => vec![
            InstallInstruction { label: "rustup".into(), command: "rustup component add rust-analyzer".into() },
            InstallInstruction { label: "Ubuntu/Debian".into(), command: "sudo apt install rust-analyzer".into() },
            InstallInstruction { label: "Fedora".into(), command: "sudo dnf install rust-analyzer".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S rust-analyzer".into() },
            InstallInstruction { label: "openSUSE".into(), command: "sudo zypper install rust-analyzer".into() },
            InstallInstruction { label: "Alpine".into(), command: "sudo apk add rust-analyzer".into() },
            InstallInstruction { label: "Void".into(), command: "sudo xbps-install rust-analyzer".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.rust-analyzer".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install rust-analyzer".into() },
            InstallInstruction { label: "pip".into(), command: "pip install rust-analyzer".into() },
        ],
        "c/cpp" => vec![
            InstallInstruction { label: "Ubuntu/Debian".into(), command: "sudo apt install clangd".into() },
            InstallInstruction { label: "Fedora".into(), command: "sudo dnf install clang-tools-extra".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S clang".into() },
            InstallInstruction { label: "openSUSE".into(), command: "sudo zypper install clang-tools".into() },
            InstallInstruction { label: "Alpine".into(), command: "sudo apk add clang-extra-tools".into() },
            InstallInstruction { label: "Void".into(), command: "sudo xbps-install clang-tools-extra".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.clang-tools".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install llvm".into() },
        ],
        "python" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g pyright".into() },
            InstallInstruction { label: "pip".into(), command: "pip install pyright".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S pyright".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.pyright".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install pyright".into() },
        ],
        "go" => vec![
            InstallInstruction { label: "go".into(), command: "go install golang.org/x/tools/gopls@latest".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S gopls".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.gopls".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install gopls".into() },
        ],
        "typescript" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g typescript-language-server typescript".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S typescript-language-server".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.typescript-language-server".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install typescript-language-server".into() },
        ],
        "java" => vec![
            InstallInstruction { label: "Ubuntu/Debian".into(), command: "sudo apt install eclipse-jdtls".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S jdtls".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.eclipse-jdtls".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install jdtls".into() },
        ],
        "ruby" => vec![
            InstallInstruction { label: "gem".into(), command: "gem install solargraph".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S solargraph".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.solargraph".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install solargraph".into() },
        ],
        "lua" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g lua-language-server".into() },
            InstallInstruction { label: "Ubuntu/Debian".into(), command: "sudo apt install lua-language-server".into() },
            InstallInstruction { label: "Fedora".into(), command: "sudo dnf install lua-language-server".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S lua-language-server".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.lua-language-server".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install lua-language-server".into() },
        ],
        "html" | "css" | "json" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g vscode-langservers-extracted".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S vscode-langservers-extracted".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.vscode-langservers-extracted".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install vscode-langservers-extracted".into() },
        ],
        "bash" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g bash-language-server".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S bash-language-server".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.bash-language-server".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install bash-language-server".into() },
        ],
        "yaml" => vec![
            InstallInstruction { label: "npm".into(), command: "npm install -g yaml-language-server".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S yaml-language-server".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.yaml-language-server".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install yaml-language-server".into() },
        ],
        "gdscript" => vec![
            InstallInstruction { label: "Godot Editor".into(), command: "Download from https://godotengine.org".into() },
            InstallInstruction { label: "Arch".into(), command: "sudo pacman -S godot".into() },
            InstallInstruction { label: "NixOS".into(), command: "nix-env -iA nixpkgs.godot".into() },
            InstallInstruction { label: "macOS".into(), command: "brew install godot".into() },
        ],
        _ => Vec::new(),
    }
}

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
#[must_use]
pub fn command_exists_on_path(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok()
}

/// Check if a binary is a rustup proxy by running `--version` and looking for
/// the "Unknown binary" error message that rustup prints for uninstalled tools.
#[must_use]
fn is_rustup_proxy(command: &str) -> bool {
    let Ok(output) = Command::new(command).arg("--version").output() else {
        return false;
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("Unknown binary")
}

/// Check whether a rustup component is actually functional by running it through
/// `rustup run stable <command> --version`. Returns `true` only if the process
/// exits successfully (exit code 0), meaning the component is installed.
#[must_use]
fn rustup_component_works(component: &str) -> bool {
    Command::new("rustup")
        .args(["run", "stable", component, "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Detect the current platform/distro for install suggestion filtering.
#[must_use]
pub fn detect_platform() -> LinuxDistro {
    if cfg!(target_os = "macos") {
        return LinuxDistro::MacOS;
    }
    if cfg!(target_os = "windows") {
        return LinuxDistro::Windows;
    }
    if !cfg!(target_os = "linux") {
        return LinuxDistro::Other;
    }

    let contents = std::fs::read_to_string("/etc/os-release").unwrap_or_default();

    if contents.contains("Ubuntu") {
        LinuxDistro::Ubuntu
    } else if contents.contains("Debian") {
        LinuxDistro::Debian
    } else if contents.contains("Fedora") {
        LinuxDistro::Fedora
    } else if contents.contains("Arch") || contents.contains("archlinux") || contents.contains("Manjaro") || contents.contains("EndeavourOS") {
        LinuxDistro::Arch
    } else if contents.contains("openSUSE") || contents.contains("SUSE") {
        LinuxDistro::OpenSuse
    } else if contents.contains("Alpine") {
        LinuxDistro::Alpine
    } else if contents.contains("Void") {
        LinuxDistro::Void
    } else if contents.contains("NixOS") {
        LinuxDistro::NixOS
    } else {
        LinuxDistro::UnknownLinux
    }
}

/// Return the best install instruction for a language given the current platform.
/// Returns `None` if no instructions are known for this language.
#[must_use]
pub fn best_install_instruction(language: &str) -> Option<InstallInstruction> {
    let distro = detect_platform();
    let instructions = install_instructions_for(language);
    if instructions.is_empty() {
        return None;
    }

    let label_match = match distro {
        LinuxDistro::Ubuntu | LinuxDistro::Debian => "Ubuntu/Debian",
        LinuxDistro::Fedora => "Fedora",
        LinuxDistro::Arch => "Arch",
        LinuxDistro::OpenSuse => "openSUSE",
        LinuxDistro::Alpine => "Alpine",
        LinuxDistro::Void => "Void",
        LinuxDistro::NixOS => "NixOS",
        LinuxDistro::MacOS => "macOS",
        LinuxDistro::Windows | LinuxDistro::UnknownLinux | LinuxDistro::Other => {
            instructions.first().map(|i| i.label.as_str()).unwrap_or("")
        }
    };

    instructions
        .iter()
        .find(|i| i.label == label_match)
        .or_else(|| instructions.first())
        .cloned()
}

/// Check which known LSP servers are missing and produce install suggestions.
/// Returns a list of `LspInstallAction` for every known language: installed,
/// missing, or rustup-proxy-missing.
#[must_use]
pub fn check_lsp_availability() -> Vec<LspInstallAction> {
    let mut actions = Vec::new();

    for desc in KNOWN_LSP_SERVERS_TABLE {
        if !command_exists_on_path(desc.command) {
            actions.push(LspInstallAction::Missing {
                language: desc.language.to_string(),
                instructions: install_instructions_for(desc.language),
            });
            continue;
        }

        if desc.command == "rust-analyzer" && is_rustup_proxy("rust-analyzer") {
            if rustup_component_works("rust-analyzer") {
                actions.push(LspInstallAction::Installed);
            } else {
                actions.push(LspInstallAction::RustupProxyMissing {
                    language: desc.language.to_string(),
                    component: "rust-analyzer".to_string(),
                });
            }
            continue;
        }

        actions.push(LspInstallAction::Installed);
    }

    actions
}

/// Format a human-readable install prompt for missing LSP servers.
#[must_use]
pub fn format_install_prompt(actions: &[LspInstallAction]) -> String {
    let mut lines = Vec::new();
    let distro = detect_platform();

    for action in actions {
        match action {
            LspInstallAction::Installed => continue,
            LspInstallAction::Missing { language, instructions } => {
                lines.push(format!("  {language}: not found"));
                let best = instructions
                    .iter()
                    .find(|i| match distro {
                        LinuxDistro::Ubuntu | LinuxDistro::Debian => i.label == "Ubuntu/Debian",
                        LinuxDistro::Fedora => i.label == "Fedora",
                        LinuxDistro::Arch => i.label == "Arch",
                        LinuxDistro::OpenSuse => i.label == "openSUSE",
                        LinuxDistro::Alpine => i.label == "Alpine",
                        LinuxDistro::Void => i.label == "Void",
                        LinuxDistro::NixOS => i.label == "NixOS",
                        LinuxDistro::MacOS => i.label == "macOS",
                        _ => false,
                    })
                    .or_else(|| instructions.first());
                if let Some(inst) = best {
                    lines.push(format!("    → {}", inst.command));
                }
                for inst in instructions {
                    if Some(inst) != best {
                        lines.push(format!("    • {} ({})", inst.command, inst.label));
                    }
                }
            }
            LspInstallAction::RustupProxyMissing { language, component } => {
                lines.push(format!("  {language}: rustup proxy found but component not installed"));
                lines.push(format!("    → rustup component add {component}"));
            }
        }
    }

    if lines.is_empty() {
        return String::new();
    }

    let mut out = "LSP servers missing — install for code intelligence:\n".to_string();
    out.push_str(&lines.join("\n"));
    out
}

/// Discover LSP servers that are actually installed on the current system.
#[must_use]
pub fn discover_available_servers() -> Vec<LspServerDescriptor> {
    KNOWN_LSP_SERVERS_TABLE
        .iter()
        .filter(|desc| command_exists_on_path(desc.command))
        .filter_map(|desc| {
            let mut server = desc.to_descriptor();
            if desc.command == "rust-analyzer" && is_rustup_proxy("rust-analyzer") {
                if rustup_component_works("rust-analyzer") {
                    server.command = "rustup".to_string();
                    server.args = vec![
                        "run".to_string(),
                        "stable".to_string(),
                        "rust-analyzer".to_string(),
                    ];
                } else {
                    return None;
                }
            }
            Some(server)
        })
        .collect()
}

/// Find the best-matching LSP server descriptor for a given file path.
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
        for server in &available {
            assert!(
                command_exists_on_path(&server.command),
                "discover_available_servers returned '{}' but command '{}' is not on PATH",
                server.language,
                server.command,
            );
        }
        let languages: Vec<&str> = available.iter().map(|s| s.language.as_str()).collect();
        if command_exists_on_path("rust-analyzer") && !is_rustup_proxy("rust-analyzer") {
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

    #[test]
    fn install_instructions_cover_all_languages() {
        for desc in KNOWN_LSP_SERVERS_TABLE {
            let instructions = install_instructions_for(desc.language);
            assert!(!instructions.is_empty(), "no install instructions for '{}'", desc.language);
        }
    }

    #[test]
    fn best_install_returns_something_for_known_languages() {
        for desc in KNOWN_LSP_SERVERS_TABLE {
            assert!(best_install_instruction(desc.language).is_some(), "no best install for '{}'", desc.language);
        }
    }

    #[test]
    fn format_install_prompt_skips_installed() {
        let actions = vec![LspInstallAction::Installed];
        let prompt = format_install_prompt(&actions);
        assert!(prompt.is_empty(), "should not prompt for installed servers");
    }

    #[test]
    fn format_install_prompt_shows_missing() {
        let actions = vec![LspInstallAction::Missing {
            language: "rust".into(),
            instructions: install_instructions_for("rust"),
        }];
        let prompt = format_install_prompt(&actions);
        assert!(prompt.contains("rust"), "should mention rust");
        assert!(prompt.contains("rustup component add rust-analyzer"), "should show rustup command");
    }

    #[test]
    fn format_install_prompt_shows_rustup_proxy_missing() {
        let actions = vec![LspInstallAction::RustupProxyMissing {
            language: "rust".into(),
            component: "rust-analyzer".into(),
        }];
        let prompt = format_install_prompt(&actions);
        assert!(prompt.contains("rustup component add rust-analyzer"));
    }

    #[test]
    fn detect_platform_returns_something() {
        let _ = detect_platform();
    }

    #[test]
    fn check_availability_returns_one_per_known_language() {
        let actions = check_lsp_availability();
        assert_eq!(actions.len(), KNOWN_LSP_SERVERS_TABLE.len());
    }

    #[test]
    fn server_descriptors_have_install_hints() {
        let servers = known_lsp_servers();
        for server in &servers {
            assert!(!server.install_hint.is_empty(), "server '{}' should have install hints", server.language);
        }
    }
}
