//! Onboarding and setup helpers.
//!
//! Detects installed tools, available providers, and provides example
//! session templates for common agent workflows.

use std::fmt;
use std::process::Command;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Provider detection
// ---------------------------------------------------------------------------

/// A detected provider credential available on the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedProvider {
    /// Provider name (e.g. "Anthropic", "OpenAI").
    pub name: String,
    /// Whether the required API key is present in the environment.
    pub key_present: bool,
    /// The env var checked.
    pub env_var: String,
    /// A hint for where to get an API key.
    pub key_url: String,
}

impl DetectedProvider {
    /// Check if a provider API key is set.
    pub fn check(name: &'static str, env_var: &'static str, key_url: &'static str) -> Self {
        let present = std::env::var(env_var).ok().map_or(false, |v| !v.is_empty());
        Self {
            name: name.to_string(),
            key_present: present,
            env_var: env_var.to_string(),
            key_url: key_url.to_string(),
        }
    }
}

impl fmt::Display for DetectedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} (env: {}, {})",
            self.name,
            if self.key_present {
                "configured"
            } else {
                "missing"
            },
            self.env_var,
            self.key_url
        )
    }
}

/// Detect all known providers.
#[must_use]
pub fn detect_providers() -> Vec<DetectedProvider> {
    vec![
        DetectedProvider::check(
            "Anthropic (Claude)",
            "ANTHROPIC_API_KEY",
            "https://console.anthropic.com/settings/keys",
        ),
        DetectedProvider::check(
            "OpenAI (GPT)",
            "OPENAI_API_KEY",
            "https://platform.openai.com/api-keys",
        ),
        DetectedProvider::check("xAI (Grok)", "XAI_API_KEY", "https://console.x.ai/"),
        DetectedProvider::check(
            "DashScope / Alibaba (Qwen)",
            "DASHSCOPE_API_KEY",
            "https://dashscope.aliyun.com/",
        ),
        DetectedProvider::check(
            "DeepSeek",
            "DEEPSEEK_API_KEY",
            "https://platform.deepseek.com/",
        ),
        DetectedProvider::check("Ollama (local)", "OLLAMA_HOST", "https://ollama.com/"),
        DetectedProvider::check("vLLM (local)", "VLLM_API_KEY", "https://docs.vllm.ai/"),
        DetectedProvider::check("Qwen (API)", "QWEN_API_KEY", "https://help.aliyun.com/"),
    ]
}

// ---------------------------------------------------------------------------
// Installed tools detection
// ---------------------------------------------------------------------------

/// A detected CLI tool available on the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedTool {
    /// Tool name.
    pub name: String,
    /// Whether it is installed and executable.
    pub installed: bool,
    /// Version string (if available).
    pub version: Option<String>,
}

impl fmt::Display for DetectedTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) if self.installed => write!(f, "{} ({})", self.name, v),
            _ if self.installed => write!(f, "{} (installed)", self.name),
            _ => write!(f, "{} (not found)", self.name),
        }
    }
}

/// Check whether a tool is on PATH and optionally return its version.
#[must_use]
pub fn check_tool(name: &str, version_flag: &str) -> DetectedTool {
    let output = Command::new(name).arg(version_flag).output();
    match output {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
                .or_else(|| {
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .map(|s| s.trim().to_string())
                });
            DetectedTool {
                name: name.to_string(),
                installed: true,
                version,
            }
        }
        _ => DetectedTool {
            name: name.to_string(),
            installed: false,
            version: None,
        },
    }
}

/// Detect all common development tools.
#[must_use]
pub fn detect_tools() -> Vec<DetectedTool> {
    vec![
        check_tool("git", "--version"),
        check_tool("docker", "--version"),
        check_tool("cargo", "--version"),
        check_tool("node", "--version"),
        check_tool("python3", "--version"),
        check_tool("curl", "--version"),
        check_tool("gh", "--version"),
    ]
}

// ---------------------------------------------------------------------------
// Setup report
// ---------------------------------------------------------------------------

/// A full setup report combining providers, tools, and next steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupReport {
    /// Detected providers.
    pub providers: Vec<DetectedProvider>,
    /// Installed tools.
    pub tools: Vec<DetectedTool>,
    /// Working directory.
    pub cwd: String,
    /// Whether at least one provider is configured.
    pub has_provider: bool,
    /// Number of tools installed.
    pub tool_count: usize,
}

impl SetupReport {
    /// Run a full setup scan from the given working directory.
    #[must_use]
    pub fn scan() -> Self {
        let providers = detect_providers();
        let tools = detect_tools();
        let has_provider = providers.iter().any(|p| p.key_present);
        let tool_count = tools.iter().filter(|t| t.installed).count();

        Self {
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string()),
            providers,
            tools,
            has_provider,
            tool_count,
        }
    }

    /// Render a human-readable setup report.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec![
            "Setup".to_string(),
            format!("  Working directory {}", self.cwd),
            String::new(),
            "Providers".to_string(),
        ];

        for p in &self.providers {
            let icon = if p.key_present {
                "configured"
            } else {
                "missing"
            };
            lines.push(format!("  {:<30} {}", p.name, icon));
        }

        lines.push(String::new());
        lines.push("Tools".to_string());
        for t in &self.tools {
            let version = t.version.as_deref().unwrap_or(if t.installed {
                "installed"
            } else {
                "not found"
            });
            lines.push(format!("  {:<30} {}", t.name, version));
        }

        lines.push(String::new());

        if !self.has_provider {
            lines.push("  No API providers configured.".to_string());
            lines.push("  Set one of these environment variables:".to_string());
            for p in &self.providers {
                if !p.key_present {
                    lines.push(format!("    {}  <- {}", p.env_var, p.key_url));
                }
            }
        } else {
            lines.push("  API provider detected. You're ready to start.".to_string());
        }

        lines.join("\n")
    }

    /// Generate an example session template for quick-start.
    #[must_use]
    pub fn example_prompt(&self) -> String {
        let provider_hint = if self.has_provider {
            ""
        } else {
            " (after setting ANTHROPIC_API_KEY)"
        };
        format!("claw prompt \"Summarize this repository\"{provider_hint}")
    }
}

// ---------------------------------------------------------------------------
// Example session templates
// ---------------------------------------------------------------------------

/// A ready-to-use session template for common agent workflows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTemplate {
    /// Template name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// System prompt for the template.
    pub system_prompt: Vec<String>,
    /// Tools to enable.
    pub tools: Vec<String>,
}

/// Get the library of built-in session templates.
#[must_use]
pub fn template_library() -> Vec<SessionTemplate> {
    vec![
        SessionTemplate {
            name: "code-review".to_string(),
            description: "Review code changes and identify bugs".to_string(),
            system_prompt: vec![
                "You are an expert code reviewer. Review the diff in the working directory."
                    .to_string(),
                "Focus on correctness, security, and edge cases. Be concise but thorough."
                    .to_string(),
            ],
            tools: vec![
                "read_file".to_string(),
                "grep_search".to_string(),
                "bash".to_string(),
            ],
        },
        SessionTemplate {
            name: "refactor".to_string(),
            description: "Refactor code while preserving behavior".to_string(),
            system_prompt: vec![
                "You are a senior software engineer. Refactor the code to improve".to_string(),
                "structure and readability without changing behavior. Run tests after".to_string(),
                "each change to verify nothing is broken.".to_string(),
            ],
            tools: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "edit_file".to_string(),
                "grep_search".to_string(),
                "glob_search".to_string(),
                "bash".to_string(),
            ],
        },
        SessionTemplate {
            name: "docs".to_string(),
            description: "Generate documentation for the codebase".to_string(),
            system_prompt: vec![
                "You are a technical writer. Read the codebase and generate".to_string(),
                "comprehensive documentation. Focus on public APIs, architecture,".to_string(),
                "and usage examples.".to_string(),
            ],
            tools: vec![
                "read_file".to_string(),
                "grep_search".to_string(),
                "glob_search".to_string(),
                "bash".to_string(),
            ],
        },
        SessionTemplate {
            name: "explore".to_string(),
            description: "Explore and summarize an unfamiliar codebase".to_string(),
            system_prompt: vec![
                "You are exploring an unfamiliar codebase. Summarize its structure,".to_string(),
                "key components, and architecture. Identify the main entry points".to_string(),
                "and data flow.".to_string(),
            ],
            tools: vec![
                "read_file".to_string(),
                "grep_search".to_string(),
                "glob_search".to_string(),
                "bash".to_string(),
            ],
        },
        SessionTemplate {
            name: "debug".to_string(),
            description: "Debug a failing test or runtime error".to_string(),
            system_prompt: vec![
                "You are debugging an issue. Reproduce the error, trace the root cause,"
                    .to_string(),
                "and propose or implement a fix. Verify by re-running the relevant tests."
                    .to_string(),
            ],
            tools: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "edit_file".to_string(),
                "grep_search".to_string(),
                "bash".to_string(),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_providers_checks_env_vars() {
        // Run in clean environment — all should be missing
        let providers = detect_providers();
        assert!(!providers.is_empty());
        for p in &providers {
            assert!(!p.name.is_empty());
            assert!(!p.env_var.is_empty());
            assert!(p.key_url.starts_with("http"));
        }
    }

    #[test]
    fn detect_tools_checks_path() {
        let tools = detect_tools();
        assert!(!tools.is_empty());
        // git should almost always be installed
        let git = tools.iter().find(|t| t.name == "git");
        assert!(git.is_some());
        assert!(git.unwrap().installed);
        assert!(git.unwrap().version.is_some());
    }

    #[test]
    fn setup_report_scan_runs_without_error() {
        let report = SetupReport::scan();
        assert!(!report.cwd.is_empty());
        assert!(!report.providers.is_empty());
        assert!(!report.tools.is_empty());
    }

    #[test]
    fn setup_report_render_includes_section_headers() {
        let report = SetupReport::scan();
        let rendered = report.render();
        assert!(rendered.contains("Providers"));
        assert!(rendered.contains("Tools"));
    }

    #[test]
    fn setup_report_example_prompt_returns_string() {
        let report = SetupReport::scan();
        let prompt = report.example_prompt();
        assert!(!prompt.is_empty());
    }

    #[test]
    fn template_library_contains_expected_templates() {
        let templates = template_library();
        assert_eq!(templates.len(), 5);
        assert!(templates.iter().any(|t| t.name == "code-review"));
        assert!(templates.iter().any(|t| t.name == "refactor"));
        assert!(templates.iter().any(|t| t.name == "explore"));
    }

    #[test]
    fn template_names_are_unique() {
        let templates = template_library();
        let mut names: Vec<String> = templates.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), templates.len());
    }

    #[test]
    fn check_tool_detects_git() {
        let result = check_tool("git", "--version");
        assert!(result.installed);
        assert!(result.version.is_some());
    }

    #[test]
    fn check_tool_reports_missing() {
        let result = check_tool("this-tool-does-not-exist-12345", "--version");
        assert!(!result.installed);
        assert!(result.version.is_none());
    }

    #[test]
    fn detected_provider_display() {
        let p = DetectedProvider::check("Test", "MISSING_VAR_XYZ", "https://example.com");
        let s = p.to_string();
        assert!(s.contains("missing"));
    }

    #[test]
    fn detected_tool_display() {
        let t = DetectedTool {
            name: "test-tool".to_string(),
            installed: true,
            version: Some("1.0".to_string()),
        };
        let s = t.to_string();
        assert!(s.contains("1.0"));
    }

    #[test]
    fn serde_round_trip_detected_provider() {
        let p = DetectedProvider::check("Anthropic", "ANTHROPIC_API_KEY", "https://example.com");
        let json = serde_json::to_string(&p).expect("serialize");
        let parsed: DetectedProvider = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, p);
    }

    #[test]
    fn serde_round_trip_setup_report() {
        let report = SetupReport::scan();
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: SetupReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.providers.len(), report.providers.len());
        assert_eq!(parsed.tools.len(), report.tools.len());
    }

    #[test]
    fn setup_report_render_conditional_provider_hint_false() {
        let report = SetupReport {
            providers: vec![DetectedProvider::check(
                "Test",
                "__TEST_MISSING_KEY__",
                "https://example.com",
            )],
            tools: vec![],
            cwd: "/tmp/test".to_string(),
            has_provider: false,
            tool_count: 0,
        };
        let rendered = report.render();
        assert!(rendered.contains("No API providers configured"));
        assert!(rendered.contains("__TEST_MISSING_KEY__"));
    }

    #[test]
    fn setup_report_render_conditional_provider_hint_true() {
        let report = SetupReport {
            providers: vec![DetectedProvider {
                name: "Test".to_string(),
                key_present: true,
                env_var: "VAR".to_string(),
                key_url: "https://example.com".to_string(),
            }],
            tools: vec![],
            cwd: "/tmp/test".to_string(),
            has_provider: true,
            tool_count: 0,
        };
        let rendered = report.render();
        assert!(rendered.contains("API provider detected"));
        assert!(!rendered.contains("No API providers configured"));
    }

    #[test]
    fn example_hint_when_no_provider_configured() {
        let report = SetupReport {
            providers: vec![],
            tools: vec![],
            cwd: "".to_string(),
            has_provider: false,
            tool_count: 0,
        };
        let prompt = report.example_prompt();
        assert!(prompt.contains("after setting ANTHROPIC_API_KEY"));
    }

    #[test]
    fn example_hint_when_provider_configured() {
        let report = SetupReport {
            providers: vec![],
            tools: vec![],
            cwd: "".to_string(),
            has_provider: true,
            tool_count: 0,
        };
        let prompt = report.example_prompt();
        assert!(!prompt.contains("after setting"));
    }

    #[test]
    fn serde_round_trip_setup_report_full_equality() {
        let report = SetupReport {
            providers: vec![DetectedProvider::check(
                "Test",
                "__TEST_MISSING_KEY__",
                "https://example.com",
            )],
            tools: vec![],
            cwd: "/tmp/test".to_string(),
            has_provider: false,
            tool_count: 0,
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: SetupReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.cwd, report.cwd);
        assert_eq!(parsed.has_provider, report.has_provider);
        assert_eq!(parsed.tool_count, report.tool_count);
        assert_eq!(parsed.providers.len(), report.providers.len());
        assert_eq!(parsed, report);
    }

    #[test]
    fn check_tool_strips_leading_whitespace() {
        let result = check_tool("git", "--version");
        assert!(result.installed);
        assert!(result.version.is_some());
        // Ensure version string doesn't start with whitespace
        if let Some(v) = &result.version {
            assert_eq!(v, v.trim_start());
        }
    }

    #[test]
    fn all_templates_present() {
        let names: Vec<String> = template_library().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"code-review".to_string()));
        assert!(names.contains(&"refactor".to_string()));
        assert!(names.contains(&"docs".to_string()));
        assert!(names.contains(&"explore".to_string()));
        assert!(names.contains(&"debug".to_string()));
    }

    #[test]
    fn check_tool_reads_stderr() {
        // python3 --version writes to stderr
        let result = check_tool("python3", "--version");
        assert!(
            result.installed,
            "python3 should be installed on this system"
        );
        assert!(
            result.version.is_some(),
            "version should be parsed from stderr fallback"
        );
        assert!(
            result.version.as_ref().unwrap().contains("Python"),
            "version line should contain 'Python'"
        );
    }

    #[test]
    fn detected_provider_with_set_env_var() {
        std::env::set_var("__TEST_SDK_PROV_XYZ__", "sk-test-value");
        let p = DetectedProvider::check("Test", "__TEST_SDK_PROV_XYZ__", "https://example.com");
        assert!(p.key_present);
        std::env::remove_var("__TEST_SDK_PROV_XYZ__");
    }

    #[test]
    fn detected_provider_with_empty_env_var() {
        std::env::set_var("__TEST_SDK_PROV_EMPTY__", "");
        let p = DetectedProvider::check("Test", "__TEST_SDK_PROV_EMPTY__", "https://example.com");
        assert!(!p.key_present, "empty value should not count as present");
        std::env::remove_var("__TEST_SDK_PROV_EMPTY__");
    }
}
