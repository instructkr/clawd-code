use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-lifetime set of already-emitted config deprecation warning strings.
/// Prevents duplicate warnings when `ConfigLoader::load()` is called multiple
/// times within a single CLI invocation. (ROADMAP #698)
static EMITTED_CONFIG_WARNINGS: std::sync::OnceLock<Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

/// When set to `true`, `emit_config_warning_once` silently drops all prose
/// deprecation warnings instead of writing them to stderr.  Set this flag
/// before any settings load when `--output-format json` is active so that
/// JSON-mode machine consumers see empty stderr on success.  (#824)
static SUPPRESS_CONFIG_WARNINGS_STDERR: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Call this once at startup when `--output-format json` is active.
pub fn suppress_config_warnings_for_json_mode() {
    SUPPRESS_CONFIG_WARNINGS_STDERR.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn emit_config_warning_once(warning: &str) {
    if SUPPRESS_CONFIG_WARNINGS_STDERR.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let set = EMITTED_CONFIG_WARNINGS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
    if guard.insert(warning.to_string()) {
        eprintln!("warning: {warning}");
    }
}

use crate::json::JsonValue;
use crate::sandbox::{FilesystemIsolationMode, SandboxConfig};

/// Schema name advertised by generated settings files.
pub const CLAW_SETTINGS_SCHEMA_NAME: &str = "SettingsSchema";

/// Origin of a loaded settings file in the configuration precedence chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSource {
    User,
    Project,
    Local,
}

/// Effective permission mode after decoding config values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// A discovered config file and the scope it contributes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub source: ConfigSource,
    pub path: PathBuf,
}

/// Fully merged runtime configuration plus parsed feature-specific views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    merged: BTreeMap<String, JsonValue>,
    loaded_entries: Vec<ConfigEntry>,
    feature_config: RuntimeFeatureConfig,
}

/// Machine-readable load state for a discovered config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFileStatus {
    Loaded,
    NotFound,
    Skipped,
    LoadError,
}

impl ConfigFileStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::NotFound => "not_found",
            Self::Skipped => "skipped",
            Self::LoadError => "load_error",
        }
    }
}

/// Structured status for one discovered config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFileReport {
    pub entry: ConfigEntry,
    pub loaded: bool,
    pub status: ConfigFileStatus,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub precedence_rank: usize,
    pub wins_for_keys: Vec<String>,
    pub shadowed_keys: Vec<String>,
    key_paths: Vec<String>,
}

/// Best-effort inspection of the config discovery and load pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigInspection {
    pub files: Vec<ConfigFileReport>,
    pub runtime_config: Option<RuntimeConfig>,
    pub warnings: Vec<String>,
    pub load_error: Option<String>,
}

/// Parsed plugin-related settings extracted from runtime config.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePluginConfig {
    enabled_plugins: BTreeMap<String, bool>,
    external_directories: Vec<String>,
    install_root: Option<String>,
    registry_path: Option<String>,
    bundled_root: Option<String>,
    max_output_tokens: Option<u32>,
}

/// API timeout and retry configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTimeoutConfig {
    /// Connect timeout in seconds. Defaults to 30.
    pub connect_timeout_secs: u64,
    /// Request timeout in seconds. Defaults to 300 (5 minutes).
    pub request_timeout_secs: u64,
    /// Maximum retry attempts on transient failures. Defaults to 8.
    pub max_retries: u32,
}

impl Default for ApiTimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 30,
            request_timeout_secs: 300,
            max_retries: 8,
        }
    }
}

/// Structured feature configuration consumed by runtime subsystems.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeFeatureConfig {
    hooks: RuntimeHookConfig,
    plugins: RuntimePluginConfig,
    mcp: McpConfigCollection,
    oauth: Option<OAuthConfig>,
    model: Option<String>,
    aliases: BTreeMap<String, String>,
    permission_mode: Option<ResolvedPermissionMode>,
    permission_rules: RuntimePermissionRuleConfig,
    sandbox: SandboxConfig,
    provider_fallbacks: ProviderFallbackConfig,
    trusted_roots: Vec<String>,
    api_timeout: ApiTimeoutConfig,
    rules_import: RulesImportConfig,
    provider: RuntimeProviderConfig,
}

/// Controls which external AI coding framework rules are imported into the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RulesImportConfig {
    /// Import from all supported frameworks when files are detected.
    #[default]
    Auto,
    /// Do not import external framework rules; keep Claw instruction files only.
    None,
    /// Import only the named frameworks.
    List(Vec<String>),
}

impl RulesImportConfig {
    #[must_use]
    pub fn should_import(&self, framework: &str) -> bool {
        match self {
            Self::Auto => true,
            Self::None => false,
            Self::List(frameworks) => frameworks
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(framework)),
        }
    }
}

/// Stored provider configuration from the setup wizard.
///
/// Represents the `provider` section in `~/.claw/settings.json`, used as a
/// fallback when environment variables are absent (3-tier resolution:
/// env var > .env file > stored config).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeProviderConfig {
    kind: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
}

impl RuntimeProviderConfig {
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Ordered chain of fallback model identifiers used when the primary
/// provider returns a retryable failure (429/500/503/etc.). The chain is
/// strict: each entry is tried in order until one succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderFallbackConfig {
    primary: Option<String>,
    fallbacks: Vec<String>,
}

/// Hook command lists grouped by lifecycle stage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeHookConfig {
    pre_tool_use: Vec<RuntimeHookCommand>,
    post_tool_use: Vec<RuntimeHookCommand>,
    post_tool_use_failure: Vec<RuntimeHookCommand>,
    invalid_hooks: Vec<RuntimeInvalidHookConfig>,
}

/// A hook command plus optional tool matcher from object-style hook config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHookCommand {
    command: String,
    matcher: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInvalidHookConfig {
    pub event: String,
    pub index: Option<usize>,
    pub hook_index: Option<usize>,
    pub kind: String,
    pub error_field: String,
    pub reason: String,
}

/// Raw permission rule lists grouped by allow, deny, and ask behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePermissionRuleConfig {
    allow: Vec<String>,
    deny: Vec<String>,
    ask: Vec<String>,
    /// #159: simple tool-name denials parsed from the `deniedTools` config field.
    /// Unlike the `deny` rules (pattern-based), `denied_tools` is a flat list of
    /// tool names that are unconditionally denied regardless of permission mode.
    denied_tools: Vec<String>,
}

/// Collection of configured MCP servers after scope-aware merging.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpConfigCollection {
    servers: BTreeMap<String, ScopedMcpServerConfig>,
    invalid_servers: Vec<McpInvalidServerConfig>,
    total_configured: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInvalidServerConfig {
    pub name: String,
    pub scope: ConfigSource,
    pub path: PathBuf,
    pub error_field: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMcpServerConfig {
    pub required: bool,
    pub scope: ConfigSource,
    pub config: McpServerConfig,
}

/// Transport families supported by configured MCP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Sse,
    Http,
    Ws,
    Sdk,
    ManagedProxy,
}

/// Scope-normalized MCP server configuration variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpRemoteServerConfig),
    Http(McpRemoteServerConfig),
    Ws(McpWebSocketServerConfig),
    Sdk(McpSdkServerConfig),
    ManagedProxy(McpManagedProxyServerConfig),
}

/// Configuration for an MCP server launched as a local stdio process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub tool_call_timeout_ms: Option<u64>,
}

/// Configuration for an MCP server reached over HTTP or SSE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    pub oauth: Option<McpOAuthConfig>,
}

/// Configuration for an MCP server reached over WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpWebSocketServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
}

/// Configuration for an MCP server addressed through an SDK name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSdkServerConfig {
    pub name: String,
}

/// Configuration for an MCP managed-proxy endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagedProxyServerConfig {
    pub url: String,
    pub id: String,
}

/// OAuth overrides associated with a remote MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthConfig {
    pub client_id: Option<String>,
    pub callback_port: Option<u16>,
    pub auth_server_metadata_url: Option<String>,
    pub xaa: Option<bool>,
}

/// OAuth client configuration used by the main Claw runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub callback_port: Option<u16>,
    pub manual_redirect_url: Option<String>,
    pub scopes: Vec<String>,
}

/// Errors raised while reading or parsing runtime configuration files.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(String),
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(
                f,
                "{error}\nFix: open the file shown above and correct the JSON syntax, then retry."
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Discovers config files and merges them into a [`RuntimeConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoader {
    cwd: PathBuf,
    config_home: PathBuf,
}

impl ConfigLoader {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, config_home: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            config_home: config_home.into(),
        }
    }

    #[must_use]
    pub fn default_for(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let config_home = default_config_home();
        Self { cwd, config_home }
    }

    #[must_use]
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    #[must_use]
    pub fn discover(&self) -> Vec<ConfigEntry> {
        let user_legacy_path = self.config_home.parent().map_or_else(
            || PathBuf::from(".claw.json"),
            |parent| parent.join(".claw.json"),
        );
        vec![
            ConfigEntry {
                source: ConfigSource::User,
                path: user_legacy_path,
            },
            ConfigEntry {
                source: ConfigSource::User,
                path: self.config_home.join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".claw.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".claw").join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Local,
                path: self.cwd.join(".claw").join("settings.local.json"),
            },
        ]
    }

    pub fn load(&self) -> Result<RuntimeConfig, ConfigError> {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp = McpConfigCollection::default();
        let mut all_warnings = Vec::new();

        for entry in self.discover() {
            crate::config_validate::check_unsupported_format(&entry.path)?;
            let OptionalConfigFile::Loaded(parsed) = read_optional_json_object(&entry.path)? else {
                continue;
            };
            let validation = crate::config_validate::validate_config_file(
                &parsed.object,
                &parsed.source,
                &entry.path,
            );
            if !validation.is_ok() {
                let first_error = &validation.errors[0];
                return Err(ConfigError::Parse(first_error.to_string()));
            }
            all_warnings.extend(validation.warnings);
            validate_optional_hooks_config(&parsed.object, &entry.path)?;
            merge_mcp_servers(&mut mcp, entry.source, &parsed.object, &entry.path)?;
            deep_merge_objects(&mut merged, &parsed.object);
            loaded_entries.push(entry);
        }

        for warning in &all_warnings {
            emit_config_warning_once(&warning.to_string());
        }

        build_runtime_config(merged, loaded_entries, mcp)
    }

    /// Like [`load`] but also returns the list of validation warnings collected during
    /// loading, without emitting them to stderr. Callers that want to surface warnings
    /// through a structured channel (e.g. the JSON config envelope) should use this.
    /// #773: enables JSON-mode callers to include `warnings` in their output envelope
    /// instead of receiving unstructured text on stderr.
    pub fn load_collecting_warnings(&self) -> Result<(RuntimeConfig, Vec<String>), ConfigError> {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp = McpConfigCollection::default();
        let mut all_warnings: Vec<String> = Vec::new();

        for entry in self.discover() {
            crate::config_validate::check_unsupported_format(&entry.path)?;
            let OptionalConfigFile::Loaded(parsed) = read_optional_json_object(&entry.path)? else {
                continue;
            };
            let validation = crate::config_validate::validate_config_file(
                &parsed.object,
                &parsed.source,
                &entry.path,
            );
            if !validation.is_ok() {
                let first_error = &validation.errors[0];
                return Err(ConfigError::Parse(first_error.to_string()));
            }
            all_warnings.extend(validation.warnings.iter().map(|w| w.to_string()));
            validate_optional_hooks_config(&parsed.object, &entry.path)?;
            merge_mcp_servers(&mut mcp, entry.source, &parsed.object, &entry.path)?;
            deep_merge_objects(&mut merged, &parsed.object);
            loaded_entries.push(entry);
        }

        let config = build_runtime_config(merged, loaded_entries, mcp)?;
        Ok((config, all_warnings))
    }

    /// Inspect every discovered config path and return per-file status details.
    /// Unlike [`Self::load`], this is best-effort: invalid files are reported in
    /// `files[]` and skipped from the merged runtime view so JSON config callers can
    /// show the whole discovery picture without collapsing every unloaded path to
    /// `loaded:false`.
    #[must_use]
    pub fn inspect_collecting_warnings(&self) -> ConfigInspection {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp = McpConfigCollection::default();
        let mut warnings = Vec::new();
        let mut files = Vec::new();
        let mut load_error = None;

        for (index, entry) in self.discover().into_iter().enumerate() {
            let precedence_rank = index + 1;
            if let Err(error) = crate::config_validate::check_unsupported_format(&entry.path) {
                let detail = error.to_string();
                load_error.get_or_insert_with(|| detail.clone());
                files.push(ConfigFileReport::load_error(
                    entry,
                    precedence_rank,
                    "unsupported_format",
                    detail,
                ));
                continue;
            }

            let parsed = match read_optional_json_object(&entry.path) {
                Ok(OptionalConfigFile::Loaded(parsed)) => parsed,
                Ok(OptionalConfigFile::NotFound) => {
                    files.push(ConfigFileReport::not_found(entry, precedence_rank));
                    continue;
                }
                Ok(OptionalConfigFile::Skipped { reason, detail }) => {
                    files.push(ConfigFileReport::skipped(
                        entry,
                        precedence_rank,
                        reason,
                        detail,
                    ));
                    continue;
                }
                Err(error) => {
                    let reason = config_error_reason(&error).to_string();
                    let detail = error.to_string();
                    load_error.get_or_insert_with(|| detail.clone());
                    files.push(ConfigFileReport::load_error(
                        entry,
                        precedence_rank,
                        reason,
                        detail,
                    ));
                    continue;
                }
            };

            let validation = crate::config_validate::validate_config_file(
                &parsed.object,
                &parsed.source,
                &entry.path,
            );
            if !validation.is_ok() {
                let detail = validation.errors[0].to_string();
                load_error.get_or_insert_with(|| detail.clone());
                files.push(ConfigFileReport::load_error(
                    entry,
                    precedence_rank,
                    "validation_error",
                    detail,
                ));
                continue;
            }
            warnings.extend(
                validation
                    .warnings
                    .iter()
                    .map(|warning| warning.to_string()),
            );

            if let Err(error) = validate_optional_hooks_config(&parsed.object, &entry.path) {
                let detail = error.to_string();
                load_error.get_or_insert_with(|| detail.clone());
                files.push(ConfigFileReport::load_error(
                    entry,
                    precedence_rank,
                    "validation_error",
                    detail,
                ));
                continue;
            }

            if let Err(error) =
                merge_mcp_servers(&mut mcp, entry.source, &parsed.object, &entry.path)
            {
                let detail = error.to_string();
                load_error.get_or_insert_with(|| detail.clone());
                files.push(ConfigFileReport::load_error(
                    entry,
                    precedence_rank,
                    "parse_error",
                    detail,
                ));
                continue;
            }

            let key_paths = collect_config_key_paths(&parsed.object);
            deep_merge_objects(&mut merged, &parsed.object);
            loaded_entries.push(entry.clone());
            files.push(ConfigFileReport::loaded(entry, precedence_rank, key_paths));
        }

        annotate_config_file_precedence(&mut files);

        let runtime_config = match build_runtime_config(merged, loaded_entries, mcp) {
            Ok(config) => Some(config),
            Err(error) => {
                load_error.get_or_insert_with(|| error.to_string());
                None
            }
        };

        ConfigInspection {
            files,
            runtime_config,
            warnings,
            load_error,
        }
    }
}

impl ConfigFileReport {
    fn loaded(entry: ConfigEntry, precedence_rank: usize, key_paths: Vec<String>) -> Self {
        Self {
            entry,
            loaded: true,
            status: ConfigFileStatus::Loaded,
            reason: None,
            detail: None,
            precedence_rank,
            wins_for_keys: Vec::new(),
            shadowed_keys: Vec::new(),
            key_paths,
        }
    }

    fn not_found(entry: ConfigEntry, precedence_rank: usize) -> Self {
        Self {
            entry,
            loaded: false,
            status: ConfigFileStatus::NotFound,
            reason: Some("not_found".to_string()),
            detail: None,
            precedence_rank,
            wins_for_keys: Vec::new(),
            shadowed_keys: Vec::new(),
            key_paths: Vec::new(),
        }
    }

    fn skipped(
        entry: ConfigEntry,
        precedence_rank: usize,
        reason: String,
        detail: Option<String>,
    ) -> Self {
        Self {
            entry,
            loaded: false,
            status: ConfigFileStatus::Skipped,
            reason: Some(reason),
            detail,
            precedence_rank,
            wins_for_keys: Vec::new(),
            shadowed_keys: Vec::new(),
            key_paths: Vec::new(),
        }
    }

    fn load_error(
        entry: ConfigEntry,
        precedence_rank: usize,
        reason: impl Into<String>,
        detail: String,
    ) -> Self {
        Self {
            entry,
            loaded: false,
            status: ConfigFileStatus::LoadError,
            reason: Some(reason.into()),
            detail: Some(detail),
            precedence_rank,
            wins_for_keys: Vec::new(),
            shadowed_keys: Vec::new(),
            key_paths: Vec::new(),
        }
    }
}

fn annotate_config_file_precedence(files: &mut [ConfigFileReport]) {
    let mut winning_file_by_key = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        if !file.loaded {
            continue;
        }
        for key in &file.key_paths {
            winning_file_by_key.insert(key.clone(), index);
        }
    }

    for (index, file) in files.iter_mut().enumerate() {
        if !file.loaded {
            continue;
        }
        let mut wins_for_keys = Vec::new();
        let mut shadowed_keys = Vec::new();
        for key in &file.key_paths {
            if winning_file_by_key.get(key).copied() == Some(index) {
                wins_for_keys.push(key.clone());
            } else {
                shadowed_keys.push(key.clone());
            }
        }
        file.wins_for_keys = wins_for_keys;
        file.shadowed_keys = shadowed_keys;
    }
}

fn collect_config_key_paths(object: &BTreeMap<String, JsonValue>) -> Vec<String> {
    let mut keys = Vec::new();
    for (key, value) in object {
        collect_config_key_paths_for_value(key, value, &mut keys);
    }
    keys
}

fn collect_config_key_paths_for_value(prefix: &str, value: &JsonValue, keys: &mut Vec<String>) {
    match value {
        JsonValue::Object(object) if !object.is_empty() => {
            for (key, nested) in object {
                collect_config_key_paths_for_value(&format!("{prefix}.{key}"), nested, keys);
            }
        }
        _ => keys.push(prefix.to_string()),
    }
}

fn build_runtime_config(
    merged: BTreeMap<String, JsonValue>,
    loaded_entries: Vec<ConfigEntry>,
    mcp: McpConfigCollection,
) -> Result<RuntimeConfig, ConfigError> {
    let merged_value = JsonValue::Object(merged.clone());

    let feature_config = RuntimeFeatureConfig {
        hooks: parse_optional_hooks_config(&merged_value)?,
        plugins: parse_optional_plugin_config(&merged_value)?,
        mcp,
        oauth: parse_optional_oauth_config(&merged_value, "merged settings.oauth")?,
        model: parse_optional_model(&merged_value),
        aliases: parse_optional_aliases(&merged_value)?,
        permission_mode: parse_optional_permission_mode(&merged_value)?,
        permission_rules: parse_optional_permission_rules(&merged_value)?,
        sandbox: parse_optional_sandbox_config(&merged_value)?,
        provider_fallbacks: parse_optional_provider_fallbacks(&merged_value)?,
        trusted_roots: parse_optional_trusted_roots(&merged_value)?,
        api_timeout: parse_optional_api_timeout_config(&merged_value)?,
        rules_import: parse_optional_rules_import(&merged_value)?,
        provider: parse_optional_provider_config(&merged_value)?,
    };

    Ok(RuntimeConfig {
        merged,
        loaded_entries,
        feature_config,
    })
}

fn config_error_reason(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::Io(io_error) if io_error.kind() == std::io::ErrorKind::PermissionDenied => {
            "permission_denied"
        }
        ConfigError::Io(_) => "io_error",
        ConfigError::Parse(_) => "parse_error",
    }
}

impl RuntimeConfig {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            merged: BTreeMap::new(),
            loaded_entries: Vec::new(),
            feature_config: RuntimeFeatureConfig::default(),
        }
    }

    #[must_use]
    pub fn merged(&self) -> &BTreeMap<String, JsonValue> {
        &self.merged
    }

    #[must_use]
    pub fn loaded_entries(&self) -> &[ConfigEntry] {
        &self.loaded_entries
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.merged.get(key)
    }

    #[must_use]
    pub fn as_json(&self) -> JsonValue {
        JsonValue::Object(self.merged.clone())
    }

    #[must_use]
    pub fn feature_config(&self) -> &RuntimeFeatureConfig {
        &self.feature_config
    }

    #[must_use]
    pub fn mcp(&self) -> &McpConfigCollection {
        &self.feature_config.mcp
    }

    #[must_use]
    pub fn hooks(&self) -> &RuntimeHookConfig {
        &self.feature_config.hooks
    }

    #[must_use]
    pub fn plugins(&self) -> &RuntimePluginConfig {
        &self.feature_config.plugins
    }

    #[must_use]
    pub fn oauth(&self) -> Option<&OAuthConfig> {
        self.feature_config.oauth.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.feature_config.model.as_deref()
    }

    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.feature_config.aliases
    }

    #[must_use]
    pub fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.feature_config.permission_mode
    }

    #[must_use]
    pub fn permission_rules(&self) -> &RuntimePermissionRuleConfig {
        &self.feature_config.permission_rules
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxConfig {
        &self.feature_config.sandbox
    }

    #[must_use]
    pub fn provider_fallbacks(&self) -> &ProviderFallbackConfig {
        &self.feature_config.provider_fallbacks
    }

    #[must_use]
    pub fn trusted_roots(&self) -> &[String] {
        &self.feature_config.trusted_roots
    }

    #[must_use]
    pub fn rules_import(&self) -> &RulesImportConfig {
        &self.feature_config.rules_import
    }

    #[must_use]
    pub fn provider(&self) -> &RuntimeProviderConfig {
        &self.feature_config.provider
    }

    /// Merge config-level default trusted roots with per-call roots.
    ///
    /// Config roots are defaults and are kept first; per-call roots extend the
    /// allowlist for a specific worker/session creation request. Duplicates are
    /// removed without reordering the first occurrence so evidence remains
    /// deterministic while avoiding repeated trust checks.
    #[must_use]
    pub fn trusted_roots_with_overrides(&self, per_call_roots: &[String]) -> Vec<String> {
        merge_trusted_roots(self.trusted_roots(), per_call_roots)
    }
}

impl RuntimeFeatureConfig {
    /// Parsed provider configuration (kind, apiKey, baseUrl, model) from
    /// merged settings.
    #[must_use]
    pub fn provider(&self) -> &RuntimeProviderConfig {
        &self.provider
    }

    #[must_use]
    pub fn with_hooks(mut self, hooks: RuntimeHookConfig) -> Self {
        self.hooks = hooks;
        self
    }

    #[must_use]
    pub fn with_plugins(mut self, plugins: RuntimePluginConfig) -> Self {
        self.plugins = plugins;
        self
    }

    #[must_use]
    pub fn hooks(&self) -> &RuntimeHookConfig {
        &self.hooks
    }

    #[must_use]
    pub fn plugins(&self) -> &RuntimePluginConfig {
        &self.plugins
    }

    #[must_use]
    pub fn mcp(&self) -> &McpConfigCollection {
        &self.mcp
    }

    #[must_use]
    pub fn oauth(&self) -> Option<&OAuthConfig> {
        self.oauth.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    #[must_use]
    pub fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.permission_mode
    }

    #[must_use]
    pub fn permission_rules(&self) -> &RuntimePermissionRuleConfig {
        &self.permission_rules
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxConfig {
        &self.sandbox
    }

    #[must_use]
    pub fn provider_fallbacks(&self) -> &ProviderFallbackConfig {
        &self.provider_fallbacks
    }

    #[must_use]
    pub fn trusted_roots(&self) -> &[String] {
        &self.trusted_roots
    }

    #[must_use]
    pub fn rules_import(&self) -> &RulesImportConfig {
        &self.rules_import
    }

    /// Merge this config's default trusted roots with per-call roots.
    #[must_use]
    pub fn trusted_roots_with_overrides(&self, per_call_roots: &[String]) -> Vec<String> {
        merge_trusted_roots(self.trusted_roots(), per_call_roots)
    }
}

fn merge_trusted_roots(config_roots: &[String], per_call_roots: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(config_roots.len() + per_call_roots.len());
    for root in config_roots.iter().chain(per_call_roots.iter()) {
        if !merged.contains(root) {
            merged.push(root.clone());
        }
    }
    merged
}

impl ProviderFallbackConfig {
    #[must_use]
    pub fn new(primary: Option<String>, fallbacks: Vec<String>) -> Self {
        Self { primary, fallbacks }
    }

    #[must_use]
    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    #[must_use]
    pub fn fallbacks(&self) -> &[String] {
        &self.fallbacks
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fallbacks.is_empty()
    }
}

impl RuntimePluginConfig {
    #[must_use]
    pub fn enabled_plugins(&self) -> &BTreeMap<String, bool> {
        &self.enabled_plugins
    }

    #[must_use]
    pub fn external_directories(&self) -> &[String] {
        &self.external_directories
    }

    #[must_use]
    pub fn install_root(&self) -> Option<&str> {
        self.install_root.as_deref()
    }

    #[must_use]
    pub fn registry_path(&self) -> Option<&str> {
        self.registry_path.as_deref()
    }

    #[must_use]
    pub fn bundled_root(&self) -> Option<&str> {
        self.bundled_root.as_deref()
    }

    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }

    pub fn set_max_output_tokens(&mut self, max_output_tokens: Option<u32>) {
        self.max_output_tokens = max_output_tokens;
    }

    pub fn set_plugin_state(&mut self, plugin_id: String, enabled: bool) {
        self.enabled_plugins.insert(plugin_id, enabled);
    }

    #[must_use]
    pub fn state_for(&self, plugin_id: &str, default_enabled: bool) -> bool {
        self.enabled_plugins
            .get(plugin_id)
            .copied()
            .unwrap_or(default_enabled)
    }
}

#[must_use]
/// Returns the default per-user config directory used by the runtime.
pub fn default_config_home() -> PathBuf {
    std::env::var_os("CLAW_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claw")))
        .unwrap_or_else(|| PathBuf::from(".claw"))
}

/// Save provider settings to the user-level `~/.claw/settings.json`.
/// Creates the file and directory if they don't exist. Sets file permissions
/// to `0o600` (owner read/write only) to protect stored API keys.
pub fn save_user_provider_settings(
    kind: &str,
    api_key: &str,
    base_url: Option<&str>,
    model: Option<&str>,
) -> Result<(), ConfigError> {
    let config_home = default_config_home();
    fs::create_dir_all(&config_home).map_err(ConfigError::Io)?;
    let settings_path = config_home.join("settings.json");

    let mut root = read_settings_root(&settings_path);

    let mut provider = serde_json::Map::new();
    provider.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    provider.insert(
        "apiKey".to_string(),
        serde_json::Value::String(api_key.to_string()),
    );
    if let Some(base_url) = base_url {
        provider.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(base_url.to_string()),
        );
    } else {
        provider.remove("baseUrl");
    }
    root.insert("provider".to_string(), serde_json::Value::Object(provider));
    if let Some(model) = model {
        root.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    } else {
        root.remove("model");
    }

    write_settings_root(&settings_path, &root)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&settings_path, perms).map_err(ConfigError::Io)?;
    }

    Ok(())
}

/// Remove the `provider` section from the user-level `~/.claw/settings.json`.
pub fn clear_user_provider_settings() -> Result<(), ConfigError> {
    let config_home = default_config_home();
    let settings_path = config_home.join("settings.json");

    if !settings_path.exists() {
        return Ok(());
    }

    let mut root = read_settings_root(&settings_path);
    if root.remove("provider").is_none() {
        return Ok(());
    }
    root.remove("model");

    write_settings_root(&settings_path, &root)?;

    Ok(())
}

fn read_settings_root(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    match fs::read_to_string(path) {
        Ok(contents) if !contents.trim().is_empty() => {
            serde_json::from_str::<serde_json::Value>(&contents)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default()
        }
        _ => serde_json::Map::new(),
    }
}

fn write_settings_root(
    path: &Path,
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigError::Io)?;
    }
    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(root.clone()))
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    fs::write(path, format!("{rendered}\n")).map_err(ConfigError::Io)
}

impl RuntimeHookCommand {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            matcher: None,
        }
    }

    #[must_use]
    pub fn with_matcher(command: impl Into<String>, matcher: Option<String>) -> Self {
        Self {
            command: command.into(),
            matcher: matcher.and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }),
        }
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    #[must_use]
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        self.matcher
            .as_deref()
            .is_none_or(|matcher| hook_matcher_matches(matcher, tool_name))
    }
}

impl RuntimeHookConfig {
    #[must_use]
    pub fn new(
        pre_tool_use: Vec<String>,
        post_tool_use: Vec<String>,
        post_tool_use_failure: Vec<String>,
    ) -> Self {
        Self::from_hook_commands(
            pre_tool_use
                .into_iter()
                .map(RuntimeHookCommand::new)
                .collect(),
            post_tool_use
                .into_iter()
                .map(RuntimeHookCommand::new)
                .collect(),
            post_tool_use_failure
                .into_iter()
                .map(RuntimeHookCommand::new)
                .collect(),
        )
    }

    #[must_use]
    pub fn from_hook_commands(
        pre_tool_use: Vec<RuntimeHookCommand>,
        post_tool_use: Vec<RuntimeHookCommand>,
        post_tool_use_failure: Vec<RuntimeHookCommand>,
    ) -> Self {
        Self {
            pre_tool_use,
            post_tool_use,
            post_tool_use_failure,
            invalid_hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn pre_tool_use(&self) -> Vec<String> {
        hook_commands(&self.pre_tool_use)
    }

    #[must_use]
    pub fn pre_tool_use_entries(&self) -> &[RuntimeHookCommand] {
        &self.pre_tool_use
    }

    #[must_use]
    pub fn post_tool_use(&self) -> Vec<String> {
        hook_commands(&self.post_tool_use)
    }

    #[must_use]
    pub fn post_tool_use_entries(&self) -> &[RuntimeHookCommand] {
        &self.post_tool_use
    }

    #[must_use]
    pub fn merged(&self, other: &Self) -> Self {
        let mut merged = self.clone();
        merged.extend(other);
        merged
    }

    pub fn extend(&mut self, other: &Self) {
        extend_unique_hook_commands(&mut self.pre_tool_use, other.pre_tool_use_entries());
        extend_unique_hook_commands(&mut self.post_tool_use, other.post_tool_use_entries());
        extend_unique_hook_commands(
            &mut self.post_tool_use_failure,
            other.post_tool_use_failure_entries(),
        );
        self.invalid_hooks
            .extend(other.invalid_hooks.iter().cloned());
    }

    #[must_use]
    pub fn post_tool_use_failure(&self) -> Vec<String> {
        hook_commands(&self.post_tool_use_failure)
    }

    #[must_use]
    pub fn post_tool_use_failure_entries(&self) -> &[RuntimeHookCommand] {
        &self.post_tool_use_failure
    }

    #[must_use]
    pub fn invalid_hooks(&self) -> &[RuntimeInvalidHookConfig] {
        &self.invalid_hooks
    }

    #[must_use]
    pub fn invalid_count(&self) -> usize {
        self.invalid_hooks.len()
    }

    #[must_use]
    pub fn has_invalid_hooks(&self) -> bool {
        !self.invalid_hooks.is_empty()
    }

    pub fn push_invalid_hook(&mut self, invalid: RuntimeInvalidHookConfig) {
        self.invalid_hooks.push(invalid);
    }
}

fn hook_commands(commands: &[RuntimeHookCommand]) -> Vec<String> {
    commands.iter().map(|entry| entry.command.clone()).collect()
}

fn hook_matcher_matches(matcher: &str, tool_name: &str) -> bool {
    matcher
        .split([',', '|'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| {
            part == "*" || part.eq_ignore_ascii_case(tool_name) || wildcard_match(part, tool_name)
        })
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return false;
    }
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    let parts = pattern.split('*').collect::<Vec<_>>();
    let mut remainder = value.as_str();
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');

    if let Some(first) = parts.first().filter(|part| !part.is_empty()) {
        if !starts_with_wildcard && !remainder.starts_with(first) {
            return false;
        }
        if let Some(index) = remainder.find(first) {
            remainder = &remainder[index + first.len()..];
        }
    }

    for part in parts.iter().skip(1).filter(|part| !part.is_empty()) {
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }

    ends_with_wildcard
        || parts
            .last()
            .is_none_or(|last| last.is_empty() || remainder.is_empty())
}

impl RuntimePermissionRuleConfig {
    #[must_use]
    pub fn new(
        allow: Vec<String>,
        deny: Vec<String>,
        ask: Vec<String>,
        denied_tools: Vec<String>,
    ) -> Self {
        Self {
            allow,
            deny,
            ask,
            denied_tools,
        }
    }

    #[must_use]
    pub fn allow(&self) -> &[String] {
        &self.allow
    }

    #[must_use]
    pub fn deny(&self) -> &[String] {
        &self.deny
    }

    #[must_use]
    pub fn ask(&self) -> &[String] {
        &self.ask
    }

    #[must_use]
    pub fn denied_tools(&self) -> &[String] {
        &self.denied_tools
    }
}

impl McpConfigCollection {
    #[must_use]
    pub fn servers(&self) -> &BTreeMap<String, ScopedMcpServerConfig> {
        &self.servers
    }

    #[must_use]
    pub fn invalid_servers(&self) -> &[McpInvalidServerConfig] {
        &self.invalid_servers
    }

    #[must_use]
    pub fn total_configured(&self) -> usize {
        self.total_configured
    }

    #[must_use]
    pub fn valid_count(&self) -> usize {
        self.servers.len()
    }

    #[must_use]
    pub fn invalid_count(&self) -> usize {
        self.invalid_servers.len()
    }

    #[must_use]
    pub fn has_invalid_servers(&self) -> bool {
        !self.invalid_servers.is_empty()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ScopedMcpServerConfig> {
        self.servers.get(name)
    }
}

impl ScopedMcpServerConfig {
    #[must_use]
    pub fn transport(&self) -> McpTransport {
        self.config.transport()
    }
}

impl McpServerConfig {
    #[must_use]
    pub fn transport(&self) -> McpTransport {
        match self {
            Self::Stdio(_) => McpTransport::Stdio,
            Self::Sse(_) => McpTransport::Sse,
            Self::Http(_) => McpTransport::Http,
            Self::Ws(_) => McpTransport::Ws,
            Self::Sdk(_) => McpTransport::Sdk,
            Self::ManagedProxy(_) => McpTransport::ManagedProxy,
        }
    }
}

/// Parsed JSON object paired with its raw source text for validation.
struct ParsedConfigFile {
    object: BTreeMap<String, JsonValue>,
    source: String,
}

enum OptionalConfigFile {
    Loaded(ParsedConfigFile),
    NotFound,
    Skipped {
        reason: String,
        detail: Option<String>,
    },
}

fn read_optional_json_object(path: &Path) -> Result<OptionalConfigFile, ConfigError> {
    let is_legacy_config = path.file_name().and_then(|name| name.to_str()) == Some(".claw.json");
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OptionalConfigFile::NotFound);
        }
        Err(error) => return Err(ConfigError::Io(error)),
    };

    if contents.trim().is_empty() {
        return Ok(OptionalConfigFile::Loaded(ParsedConfigFile {
            object: BTreeMap::new(),
            source: contents,
        }));
    }

    let parsed = match JsonValue::parse(&contents) {
        Ok(parsed) => parsed,
        Err(error) if is_legacy_config => {
            return Ok(OptionalConfigFile::Skipped {
                reason: "legacy_invalid_json".to_string(),
                detail: Some(format!("{}: {error}", path.display())),
            });
        }
        Err(error) => return Err(ConfigError::Parse(format!("{}: {error}", path.display()))),
    };
    let Some(object) = parsed.as_object() else {
        if is_legacy_config {
            return Ok(OptionalConfigFile::Skipped {
                reason: "legacy_non_object".to_string(),
                detail: Some(format!(
                    "{}: top-level legacy settings value is not a JSON object",
                    path.display()
                )),
            });
        }
        return Err(ConfigError::Parse(format!(
            "{}: top-level settings value must be a JSON object",
            path.display()
        )));
    };
    Ok(OptionalConfigFile::Loaded(ParsedConfigFile {
        object: object.clone(),
        source: contents,
    }))
}

fn merge_mcp_servers(
    target: &mut McpConfigCollection,
    source: ConfigSource,
    root: &BTreeMap<String, JsonValue>,
    path: &Path,
) -> Result<(), ConfigError> {
    let Some(mcp_servers) = root.get("mcpServers") else {
        return Ok(());
    };
    let servers = expect_object(mcp_servers, &format!("{}: mcpServers", path.display()))?;
    target.total_configured += servers.len();
    for (name, value) in servers {
        let context = format!("{}: mcpServers.{name}", path.display());
        let Ok(object) = expect_object(value, &context) else {
            let error = expect_object(value, &context).expect_err("object parse must fail");
            target.servers.remove(name);
            target
                .invalid_servers
                .push(mcp_invalid_server(name, source, path, &context, &error));
            continue;
        };
        let required = match optional_bool(object, "required", &context) {
            Ok(required) => required.unwrap_or(false),
            Err(error) => {
                target.servers.remove(name);
                target
                    .invalid_servers
                    .push(mcp_invalid_server(name, source, path, &context, &error));
                continue;
            }
        };
        if let Err(error) = validate_mcp_server_keys(name, object, &context) {
            target.servers.remove(name);
            target
                .invalid_servers
                .push(mcp_invalid_server(name, source, path, &context, &error));
            continue;
        }
        let parsed = match parse_mcp_server_config(name, value, &context) {
            Ok(parsed) => parsed,
            Err(error) => {
                target.servers.remove(name);
                target
                    .invalid_servers
                    .push(mcp_invalid_server(name, source, path, &context, &error));
                continue;
            }
        };
        target.servers.insert(
            name.clone(),
            ScopedMcpServerConfig {
                required,
                scope: source,
                config: parsed,
            },
        );
    }
    Ok(())
}

fn mcp_invalid_server(
    name: &str,
    source: ConfigSource,
    path: &Path,
    context: &str,
    error: &ConfigError,
) -> McpInvalidServerConfig {
    let reason = config_error_detail(error);
    McpInvalidServerConfig {
        name: name.to_string(),
        scope: source,
        path: path.to_path_buf(),
        error_field: mcp_error_field(name, context, &reason),
        reason,
    }
}

fn config_error_detail(error: &ConfigError) -> String {
    match error {
        ConfigError::Io(error) => error.to_string(),
        ConfigError::Parse(reason) => reason.clone(),
    }
}

fn mcp_error_field(name: &str, context: &str, reason: &str) -> String {
    if let Some(field) = reason
        .split("missing string field ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
    {
        return field
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .to_string();
    }
    if let Some(field) = reason
        .split("field ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
    {
        return field
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .to_string();
    }
    reason
        .split_once(context)
        .and_then(|(_, tail)| tail.trim_start_matches('.').split(':').next())
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("mcpServers.{name}"))
}

fn validate_mcp_server_keys(
    server_name: &str,
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<(), ConfigError> {
    let server_type =
        optional_string(object, "type", context)?.unwrap_or_else(|| infer_mcp_server_type(object));
    let allowed = match server_type {
        "stdio" => &[
            "type",
            "command",
            "args",
            "env",
            "toolCallTimeoutMs",
            "required",
        ][..],
        "sse" | "http" => &[
            "type",
            "url",
            "headers",
            "headersHelper",
            "oauth",
            "required",
        ][..],
        "ws" => &["type", "url", "headers", "headersHelper", "required"][..],
        "sdk" => &["type", "name", "required"][..],
        "claudeai-proxy" => &["type", "url", "id", "required"][..],
        other => {
            return Err(ConfigError::Parse(format!(
                "{context}: unsupported MCP server type for {server_name}: {other}"
            )));
        }
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ConfigError::Parse(format!(
            "{context}: unknown MCP server field {key}"
        )));
    }
    Ok(())
}

fn parse_optional_model(root: &JsonValue) -> Option<String> {
    root.as_object()
        .and_then(|object| object.get("model"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_optional_aliases(root: &JsonValue) -> Result<BTreeMap<String, String>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(optional_string_map(object, "aliases", "merged settings")?.unwrap_or_default())
}

fn parse_optional_hooks_config(root: &JsonValue) -> Result<RuntimeHookConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimeHookConfig::default());
    };
    parse_optional_hooks_config_object(object, "merged settings.hooks")
}

fn parse_optional_hooks_config_object(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<RuntimeHookConfig, ConfigError> {
    let Some(hooks_value) = object.get("hooks") else {
        return Ok(RuntimeHookConfig::default());
    };
    let hooks = expect_object(hooks_value, context)?;
    Ok(parse_hooks_object_partial(hooks, context))
}

fn parse_hooks_object_partial(
    hooks: &BTreeMap<String, JsonValue>,
    context: &str,
) -> RuntimeHookConfig {
    let mut config = RuntimeHookConfig::default();
    parse_hook_event_partial(
        &mut config,
        hooks,
        "PreToolUse",
        context,
        |config, command| {
            config.pre_tool_use.push(command);
        },
    );
    parse_hook_event_partial(
        &mut config,
        hooks,
        "PostToolUse",
        context,
        |config, command| {
            config.post_tool_use.push(command);
        },
    );
    parse_hook_event_partial(
        &mut config,
        hooks,
        "PostToolUseFailure",
        context,
        |config, command| {
            config.post_tool_use_failure.push(command);
        },
    );
    for event in hooks.keys().filter(|event| !is_supported_hook_event(event)) {
        config.push_invalid_hook(RuntimeInvalidHookConfig {
            event: event.clone(),
            index: None,
            hook_index: None,
            kind: "unknown_hook_event".to_string(),
            error_field: event.clone(),
            reason: format!("{context}: unknown hook event {event}"),
        });
    }
    config
}

fn is_supported_hook_event(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure")
}

fn parse_hook_event_partial(
    config: &mut RuntimeHookConfig,
    hooks: &BTreeMap<String, JsonValue>,
    event: &str,
    context: &str,
    mut push_command: impl FnMut(&mut RuntimeHookConfig, RuntimeHookCommand),
) {
    let Some(value) = hooks.get(event) else {
        return;
    };
    let Some(array) = value.as_array() else {
        config.push_invalid_hook(RuntimeInvalidHookConfig {
            event: event.to_string(),
            index: None,
            hook_index: None,
            kind: "invalid_hooks_config".to_string(),
            error_field: event.to_string(),
            reason: format!("{context}: field {event} must be an array"),
        });
        return;
    };

    for (index, item) in array.iter().enumerate() {
        if let Some(command) = item.as_str() {
            if command.trim().is_empty() {
                config.push_invalid_hook(RuntimeInvalidHookConfig {
                    event: event.to_string(),
                    index: Some(index),
                    hook_index: None,
                    kind: "invalid_hooks_config".to_string(),
                    error_field: "command".to_string(),
                    reason: format!("{context}: field {event}[{index}] must be a non-empty string"),
                });
            } else {
                push_command(config, RuntimeHookCommand::new(command.to_string()));
            }
            continue;
        }

        let Some(entry) = item.as_object() else {
            config.push_invalid_hook(RuntimeInvalidHookConfig {
                event: event.to_string(),
                index: Some(index),
                hook_index: None,
                kind: "invalid_hooks_config".to_string(),
                error_field: event.to_string(),
                reason: format!(
                    "{context}: field {event}[{index}] must be a string or hook object"
                ),
            });
            continue;
        };

        let matcher = match optional_hook_matcher(entry, context, event, index) {
            Ok(matcher) => matcher,
            Err(error) => {
                config.push_invalid_hook(runtime_invalid_hook(
                    event,
                    Some(index),
                    None,
                    "matcher",
                    error,
                ));
                continue;
            }
        };
        let Some(hook_array) = entry.get("hooks").and_then(JsonValue::as_array) else {
            config.push_invalid_hook(RuntimeInvalidHookConfig {
                event: event.to_string(),
                index: Some(index),
                hook_index: None,
                kind: "invalid_hooks_config".to_string(),
                error_field: "hooks".to_string(),
                reason: format!("{context}: field {event}[{index}].hooks must be an array"),
            });
            continue;
        };
        for (hook_index, hook) in hook_array.iter().enumerate() {
            let Some(hook_object) = hook.as_object() else {
                config.push_invalid_hook(RuntimeInvalidHookConfig {
                    event: event.to_string(),
                    index: Some(index),
                    hook_index: Some(hook_index),
                    kind: "invalid_hooks_config".to_string(),
                    error_field: "hooks".to_string(),
                    reason: format!(
                        "{context}: field {event}[{index}].hooks[{hook_index}] must be an object"
                    ),
                });
                continue;
            };
            if let Some(hook_type) = hook_object.get("type") {
                let Some(hook_type) = hook_type.as_str() else {
                    config.push_invalid_hook(RuntimeInvalidHookConfig {
                        event: event.to_string(),
                        index: Some(index),
                        hook_index: Some(hook_index),
                        kind: "invalid_hooks_config".to_string(),
                        error_field: "type".to_string(),
                        reason: format!(
                            "{context}: field {event}[{index}].hooks[{hook_index}].type must be a string"
                        ),
                    });
                    continue;
                };
                if hook_type != "command" {
                    config.push_invalid_hook(RuntimeInvalidHookConfig {
                        event: event.to_string(),
                        index: Some(index),
                        hook_index: Some(hook_index),
                        kind: "invalid_hooks_config".to_string(),
                        error_field: "type".to_string(),
                        reason: format!(
                            "{context}: field {event}[{index}].hooks[{hook_index}].type must be \"command\""
                        ),
                    });
                    continue;
                }
            }
            let Some(command) = hook_object
                .get("command")
                .and_then(JsonValue::as_str)
                .filter(|command| !command.trim().is_empty())
            else {
                config.push_invalid_hook(RuntimeInvalidHookConfig {
                    event: event.to_string(),
                    index: Some(index),
                    hook_index: Some(hook_index),
                    kind: "invalid_hooks_config".to_string(),
                    error_field: "command".to_string(),
                    reason: format!(
                        "{context}: field {event}[{index}].hooks[{hook_index}].command must be a non-empty string"
                    ),
                });
                continue;
            };
            push_command(
                config,
                RuntimeHookCommand::with_matcher(command.to_string(), matcher.clone()),
            );
        }
    }
}

fn runtime_invalid_hook(
    event: &str,
    index: Option<usize>,
    hook_index: Option<usize>,
    error_field: &str,
    error: ConfigError,
) -> RuntimeInvalidHookConfig {
    RuntimeInvalidHookConfig {
        event: event.to_string(),
        index,
        hook_index,
        kind: "invalid_hooks_config".to_string(),
        error_field: error_field.to_string(),
        reason: config_error_detail(&error),
    }
}

fn validate_optional_hooks_config(
    root: &BTreeMap<String, JsonValue>,
    path: &Path,
) -> Result<(), ConfigError> {
    parse_optional_hooks_config_object(root, &format!("{}: hooks", path.display())).map(|_| ())
}

fn parse_optional_permission_rules(
    root: &JsonValue,
) -> Result<RuntimePermissionRuleConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimePermissionRuleConfig::default());
    };
    let Some(permissions) = object.get("permissions").and_then(JsonValue::as_object) else {
        return Ok(RuntimePermissionRuleConfig::default());
    };

    Ok(RuntimePermissionRuleConfig {
        allow: optional_string_array(permissions, "allow", "merged settings.permissions")?
            .unwrap_or_default(),
        deny: optional_string_array(permissions, "deny", "merged settings.permissions")?
            .unwrap_or_default(),
        ask: optional_string_array(permissions, "ask", "merged settings.permissions")?
            .unwrap_or_default(),
        denied_tools: optional_string_array(
            permissions,
            "deniedTools",
            "merged settings.permissions",
        )?
        .unwrap_or_default(),
    })
}

fn parse_optional_plugin_config(root: &JsonValue) -> Result<RuntimePluginConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimePluginConfig::default());
    };

    let mut config = RuntimePluginConfig::default();
    if let Some(enabled_plugins) = object.get("enabledPlugins") {
        config.enabled_plugins = parse_bool_map(enabled_plugins, "merged settings.enabledPlugins")?;
    }

    let Some(plugins_value) = object.get("plugins") else {
        return Ok(config);
    };
    let plugins = expect_object(plugins_value, "merged settings.plugins")?;

    if let Some(enabled_value) = plugins.get("enabled") {
        config.enabled_plugins = parse_bool_map(enabled_value, "merged settings.plugins.enabled")?;
    }
    config.external_directories =
        optional_string_array(plugins, "externalDirectories", "merged settings.plugins")?
            .unwrap_or_default();
    config.install_root =
        optional_string(plugins, "installRoot", "merged settings.plugins")?.map(str::to_string);
    config.registry_path =
        optional_string(plugins, "registryPath", "merged settings.plugins")?.map(str::to_string);
    config.bundled_root =
        optional_string(plugins, "bundledRoot", "merged settings.plugins")?.map(str::to_string);
    config.max_output_tokens = optional_u32(plugins, "maxOutputTokens", "merged settings.plugins")?;
    Ok(config)
}

fn parse_optional_permission_mode(
    root: &JsonValue,
) -> Result<Option<ResolvedPermissionMode>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(None);
    };
    if let Some(mode) = object.get("permissionMode").and_then(JsonValue::as_str) {
        return parse_permission_mode_label(mode, "merged settings.permissionMode").map(Some);
    }
    let Some(mode) = object
        .get("permissions")
        .and_then(JsonValue::as_object)
        .and_then(|permissions| permissions.get("defaultMode"))
        .and_then(JsonValue::as_str)
    else {
        return Ok(None);
    };
    parse_permission_mode_label(mode, "merged settings.permissions.defaultMode").map(Some)
}

fn parse_permission_mode_label(
    mode: &str,
    context: &str,
) -> Result<ResolvedPermissionMode, ConfigError> {
    match mode {
        "default" | "plan" | "read-only" => Ok(ResolvedPermissionMode::ReadOnly),
        "acceptEdits" | "auto" | "workspace-write" => Ok(ResolvedPermissionMode::WorkspaceWrite),
        "dontAsk" | "danger-full-access" => Ok(ResolvedPermissionMode::DangerFullAccess),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported permission mode {other}"
        ))),
    }
}

fn parse_optional_sandbox_config(root: &JsonValue) -> Result<SandboxConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(SandboxConfig::default());
    };
    let Some(sandbox_value) = object.get("sandbox") else {
        return Ok(SandboxConfig::default());
    };
    let sandbox = expect_object(sandbox_value, "merged settings.sandbox")?;
    let filesystem_mode = optional_string(sandbox, "filesystemMode", "merged settings.sandbox")?
        .map(parse_filesystem_mode_label)
        .transpose()?;
    Ok(SandboxConfig {
        enabled: optional_bool(sandbox, "enabled", "merged settings.sandbox")?,
        namespace_restrictions: optional_bool(
            sandbox,
            "namespaceRestrictions",
            "merged settings.sandbox",
        )?,
        network_isolation: optional_bool(sandbox, "networkIsolation", "merged settings.sandbox")?,
        filesystem_mode,
        allowed_mounts: optional_string_array(sandbox, "allowedMounts", "merged settings.sandbox")?
            .unwrap_or_default(),
    })
}

fn parse_optional_provider_fallbacks(
    root: &JsonValue,
) -> Result<ProviderFallbackConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(ProviderFallbackConfig::default());
    };
    let Some(value) = object.get("providerFallbacks") else {
        return Ok(ProviderFallbackConfig::default());
    };
    let entry = expect_object(value, "merged settings.providerFallbacks")?;
    let primary =
        optional_string(entry, "primary", "merged settings.providerFallbacks")?.map(str::to_string);
    let fallbacks = optional_string_array(entry, "fallbacks", "merged settings.providerFallbacks")?
        .unwrap_or_default();
    Ok(ProviderFallbackConfig { primary, fallbacks })
}

fn parse_optional_api_timeout_config(root: &JsonValue) -> Result<ApiTimeoutConfig, ConfigError> {
    let Some(timeout_value) = root.as_object().and_then(|obj| obj.get("apiTimeout")) else {
        return Ok(ApiTimeoutConfig::default());
    };
    let Some(obj) = timeout_value.as_object() else {
        return Ok(ApiTimeoutConfig::default());
    };
    let context = "merged settings.apiTimeout";
    let connect_timeout_secs = optional_u64(obj, "connectTimeout", context)?.unwrap_or(30);
    let request_timeout_secs = optional_u64(obj, "requestTimeout", context)?.unwrap_or(300);
    let max_retries = optional_u64(obj, "maxRetries", context)?
        .map(|v| v as u32)
        .unwrap_or(8);
    Ok(ApiTimeoutConfig {
        connect_timeout_secs,
        request_timeout_secs,
        max_retries,
    })
}

fn parse_optional_trusted_roots(root: &JsonValue) -> Result<Vec<String>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(Vec::new());
    };
    Ok(
        optional_string_array(object, "trustedRoots", "merged settings.trustedRoots")?
            .unwrap_or_default(),
    )
}

fn parse_optional_rules_import(root: &JsonValue) -> Result<RulesImportConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RulesImportConfig::default());
    };
    let Some(value) = object.get("rulesImport") else {
        return Ok(RulesImportConfig::default());
    };

    match value {
        JsonValue::String(value) if value.eq_ignore_ascii_case("auto") => Ok(RulesImportConfig::Auto),
        JsonValue::String(value) if value.eq_ignore_ascii_case("none") => Ok(RulesImportConfig::None),
        JsonValue::String(value) => Err(ConfigError::Parse(format!(
            "merged settings.rulesImport: expected \"auto\", \"none\", or an array of framework names, got \"{value}\""
        ))),
        JsonValue::Array(values) => values
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    ConfigError::Parse(
                        "merged settings.rulesImport: array entries must be strings".to_string(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(RulesImportConfig::List),
        _ => Err(ConfigError::Parse(
            "merged settings.rulesImport: expected \"auto\", \"none\", or an array of framework names".to_string(),
        )),
    }
}

fn parse_optional_provider_config(root: &JsonValue) -> Result<RuntimeProviderConfig, ConfigError> {
    let Some(provider_value) = root.as_object().and_then(|object| object.get("provider")) else {
        return Ok(RuntimeProviderConfig::default());
    };
    let Some(object) = provider_value.as_object() else {
        return Ok(RuntimeProviderConfig::default());
    };
    let kind = optional_string(object, "kind", "provider")?.map(str::to_string);
    let api_key = optional_string(object, "apiKey", "provider")?.map(str::to_string);
    let base_url = optional_string(object, "baseUrl", "provider")?.map(str::to_string);
    let model = optional_string(object, "model", "provider")?.map(str::to_string);
    Ok(RuntimeProviderConfig {
        kind,
        api_key,
        base_url,
        model,
    })
}

fn parse_filesystem_mode_label(value: &str) -> Result<FilesystemIsolationMode, ConfigError> {
    match value {
        "off" => Ok(FilesystemIsolationMode::Off),
        "workspace-only" => Ok(FilesystemIsolationMode::WorkspaceOnly),
        "allow-list" => Ok(FilesystemIsolationMode::AllowList),
        other => Err(ConfigError::Parse(format!(
            "merged settings.sandbox.filesystemMode: unsupported filesystem mode {other}"
        ))),
    }
}

fn parse_optional_oauth_config(
    root: &JsonValue,
    context: &str,
) -> Result<Option<OAuthConfig>, ConfigError> {
    let Some(oauth_value) = root.as_object().and_then(|object| object.get("oauth")) else {
        return Ok(None);
    };
    let object = expect_object(oauth_value, context)?;
    let client_id = expect_string(object, "clientId", context)?.to_string();
    let authorize_url = expect_string(object, "authorizeUrl", context)?.to_string();
    let token_url = expect_string(object, "tokenUrl", context)?.to_string();
    let callback_port = optional_u16(object, "callbackPort", context)?;
    let manual_redirect_url =
        optional_string(object, "manualRedirectUrl", context)?.map(str::to_string);
    let scopes = optional_string_array(object, "scopes", context)?.unwrap_or_default();
    Ok(Some(OAuthConfig {
        client_id,
        authorize_url,
        token_url,
        callback_port,
        manual_redirect_url,
        scopes,
    }))
}

/// #92: expand `${VAR}` environment variable references and `~/` home directory
/// prefix in a config string value. Returns the expanded string.
fn expand_config_value(value: &str) -> String {
    // Expand ${VAR} and $VAR references from the environment
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'{') {
                // ${VAR} form
                chars.next(); // consume '{'
                let mut var_name = String::new();
                for ch in chars.by_ref() {
                    if ch == '}' {
                        break;
                    }
                    var_name.push(ch);
                }
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val);
                }
            } else {
                // Bare $ — pass through
                result.push(c);
            }
        } else if c == '~' && result.is_empty() {
            // ~/... home directory expansion
            if let Ok(home) = std::env::var("HOME") {
                result.push_str(&home);
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn parse_mcp_server_config(
    server_name: &str,
    value: &JsonValue,
    context: &str,
) -> Result<McpServerConfig, ConfigError> {
    let object = expect_object(value, context)?;
    let server_type =
        optional_string(object, "type", context)?.unwrap_or_else(|| infer_mcp_server_type(object));
    match server_type {
        // #92: expand ${VAR} and ~/ in command, args, and url fields
        "stdio" => Ok(McpServerConfig::Stdio(McpStdioServerConfig {
            command: expand_config_value(expect_non_empty_string(object, "command", context)?),
            args: optional_string_array(object, "args", context)?
                .unwrap_or_default()
                .iter()
                .map(|a| expand_config_value(a))
                .collect(),
            env: optional_string_map(object, "env", context)?.unwrap_or_default(),
            tool_call_timeout_ms: optional_u64(object, "toolCallTimeoutMs", context)?,
        })),
        "sse" => Ok(McpServerConfig::Sse(parse_mcp_remote_server_config(
            object, context,
        )?)),
        "http" => Ok(McpServerConfig::Http(parse_mcp_remote_server_config(
            object, context,
        )?)),
        "ws" => Ok(McpServerConfig::Ws(McpWebSocketServerConfig {
            // #92: expand ${VAR} and ~/ in URL
            url: expand_config_value(expect_string(object, "url", context)?),
            headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
            headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
        })),
        "sdk" => Ok(McpServerConfig::Sdk(McpSdkServerConfig {
            name: expect_string(object, "name", context)?.to_string(),
        })),
        "claudeai-proxy" => Ok(McpServerConfig::ManagedProxy(McpManagedProxyServerConfig {
            // #92: expand ${VAR} and ~/ in URL
            url: expand_config_value(expect_string(object, "url", context)?),
            id: expect_string(object, "id", context)?.to_string(),
        })),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported MCP server type for {server_name}: {other}"
        ))),
    }
}

fn infer_mcp_server_type(object: &BTreeMap<String, JsonValue>) -> &'static str {
    if object.contains_key("url") {
        "http"
    } else {
        "stdio"
    }
}

fn parse_mcp_remote_server_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<McpRemoteServerConfig, ConfigError> {
    Ok(McpRemoteServerConfig {
        // #92: expand ${VAR} and ~/ in URL
        url: expand_config_value(expect_string(object, "url", context)?),
        headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
        headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
        oauth: parse_optional_mcp_oauth_config(object, context)?,
    })
}

fn parse_optional_mcp_oauth_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<Option<McpOAuthConfig>, ConfigError> {
    let Some(value) = object.get("oauth") else {
        return Ok(None);
    };
    let oauth = expect_object(value, &format!("{context}.oauth"))?;
    Ok(Some(McpOAuthConfig {
        client_id: optional_string(oauth, "clientId", context)?.map(str::to_string),
        callback_port: optional_u16(oauth, "callbackPort", context)?,
        auth_server_metadata_url: optional_string(oauth, "authServerMetadataUrl", context)?
            .map(str::to_string),
        xaa: optional_bool(oauth, "xaa", context)?,
    }))
}

fn expect_object<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, ConfigError> {
    value
        .as_object()
        .ok_or_else(|| ConfigError::Parse(format!("{context}: expected JSON object")))
}

fn expect_non_empty_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<&'a str, ConfigError> {
    let value = expect_string(object, key, context)?;
    if value.trim().is_empty() {
        return Err(ConfigError::Parse(format!(
            "{context}: field {key} must be a non-empty string"
        )));
    }
    Ok(value)
}

fn expect_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<&'a str, ConfigError> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| ConfigError::Parse(format!("{context}: missing string field {key}")))
}

fn optional_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<&'a str>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a string"))),
        None => Ok(None),
    }
}

fn optional_bool(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a boolean"))),
        None => Ok(None),
    }
}

fn optional_u16(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u16>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an integer"
                )));
            };
            let number = u16::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn optional_u32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u32>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be a non-negative integer"
                )));
            };
            let number = u32::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn optional_u64(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u64>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be a non-negative integer"
                )));
            };
            let number = u64::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn parse_bool_map(value: &JsonValue, context: &str) -> Result<BTreeMap<String, bool>, ConfigError> {
    let Some(map) = value.as_object() else {
        return Err(ConfigError::Parse(format!(
            "{context}: expected JSON object"
        )));
    };
    map.iter()
        .map(|(key, value)| {
            value
                .as_bool()
                .map(|enabled| (key.clone(), enabled))
                .ok_or_else(|| {
                    ConfigError::Parse(format!("{context}: field {key} must be a boolean"))
                })
        })
        .collect()
}

fn optional_string_array(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<Vec<String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(array) = value.as_array() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an array"
                )));
            };
            array
                .iter()
                .map(|item| {
                    item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        ConfigError::Parse(format!(
                            "{context}: field {key} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

fn optional_hook_matcher(
    entry: &BTreeMap<String, JsonValue>,
    context: &str,
    key: &str,
    index: usize,
) -> Result<Option<String>, ConfigError> {
    entry
        .get("matcher")
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                ConfigError::Parse(format!(
                    "{context}: field {key}[{index}].matcher must be a string"
                ))
            })
        })
        .transpose()
}

fn extend_unique_hook_commands(
    target: &mut Vec<RuntimeHookCommand>,
    values: &[RuntimeHookCommand],
) {
    for value in values {
        if !target.iter().any(|existing| existing == value) {
            target.push(value.clone());
        }
    }
}

fn optional_string_map(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(map) = value.as_object() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an object"
                )));
            };
            map.iter()
                .map(|(entry_key, entry_value)| {
                    entry_value
                        .as_str()
                        .map(|text| (entry_key.clone(), text.to_string()))
                        .ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{context}: field {key} must contain only string values"
                            ))
                        })
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

fn deep_merge_objects(
    target: &mut BTreeMap<String, JsonValue>,
    source: &BTreeMap<String, JsonValue>,
) {
    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(JsonValue::Object(existing)), JsonValue::Object(incoming)) => {
                deep_merge_objects(existing, incoming);
            }
            // #106: concatenate arrays instead of replacing
            (Some(JsonValue::Array(existing)), JsonValue::Array(incoming)) => {
                existing.extend(incoming.iter().cloned());
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deep_merge_objects, parse_permission_mode_label, ConfigFileStatus, ConfigLoader,
        ConfigSource, McpServerConfig, McpTransport, ResolvedPermissionMode, RuntimeFeatureConfig,
        RuntimeHookCommand, RuntimeHookConfig, RuntimePluginConfig, CLAW_SETTINGS_SCHEMA_NAME,
    };
    use crate::json::JsonValue;
    use crate::sandbox::FilesystemIsolationMode;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        // #149: previously used `runtime-config-{nanos}` which collided
        // under parallel `cargo test --workspace` when multiple tests
        // started within the same nanosecond bucket on fast machines.
        // Add process id + a monotonically-incrementing atomic counter
        // so every callsite gets a provably-unique directory regardless
        // of clock resolution or scheduling.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("runtime-config-{pid}-{nanos}-{seq}"))
    }

    #[test]
    fn rejects_non_object_settings_files() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "[]").expect("write bad settings");

        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");
        assert!(error
            .to_string()
            .contains("top-level settings value must be a JSON object"));

        if root.exists() {
            fs::remove_dir_all(root).expect("cleanup temp dir");
        }
    }

    #[test]
    fn loads_and_merges_claude_code_config_files_by_precedence() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.parent().expect("home parent").join(".claw.json"),
            r#"{"model":"haiku","env":{"A":"1"},"mcpServers":{"home":{"command":"uvx","args":["home"]}}}"#,
        )
        .expect("write user compat config");
        fs::write(
            home.join("settings.json"),
            r#"{"model":"sonnet","env":{"A2":"1"},"hooks":{"PreToolUse":["base"]},"permissions":{"defaultMode":"plan","allow":["Read"],"deny":["Bash(rm -rf)"]}}"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw.json"),
            r#"{"model":"project-compat","env":{"B":"2"}}"#,
        )
        .expect("write project compat config");
        fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"env":{"C":"3"},"hooks":{"PostToolUse":["project"],"PostToolUseFailure":["project-failure"]},"permissions":{"ask":["Edit"]},"mcpServers":{"project":{"command":"uvx","args":["project"]}}}"#,
        )
        .expect("write project settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"model":"opus","permissionMode":"acceptEdits"}"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(CLAW_SETTINGS_SCHEMA_NAME, "SettingsSchema");
        assert_eq!(loaded.loaded_entries().len(), 5);
        assert_eq!(loaded.loaded_entries()[0].source, ConfigSource::User);
        assert_eq!(
            loaded.get("model"),
            Some(&JsonValue::String("opus".to_string()))
        );
        assert_eq!(loaded.model(), Some("opus"));
        assert_eq!(
            loaded.permission_mode(),
            Some(ResolvedPermissionMode::WorkspaceWrite)
        );
        assert_eq!(
            loaded
                .get("env")
                .and_then(JsonValue::as_object)
                .expect("env object")
                .len(),
            4
        );
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PreToolUse"));
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PostToolUse"));
        assert_eq!(loaded.hooks().pre_tool_use(), &["base".to_string()]);
        assert_eq!(loaded.hooks().post_tool_use(), &["project".to_string()]);
        assert_eq!(
            loaded.hooks().post_tool_use_failure(),
            &["project-failure".to_string()]
        );
        assert_eq!(loaded.permission_rules().allow(), &["Read".to_string()]);
        assert_eq!(
            loaded.permission_rules().deny(),
            &["Bash(rm -rf)".to_string()]
        );
        assert_eq!(loaded.permission_rules().ask(), &["Edit".to_string()]);
        assert!(loaded.mcp().get("home").is_some());
        assert!(loaded.mcp().get("project").is_some());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_object_style_hook_entries_with_matchers() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":["legacy",{"matcher":"Bash","hooks":[{"type":"command","command":"bash-one"},{"type":"command","command":"bash-two"}]},{"matcher":"Read*","hooks":[{"command":"read-any"}]}]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.hooks().pre_tool_use(),
            vec![
                "legacy".to_string(),
                "bash-one".to_string(),
                "bash-two".to_string(),
                "read-any".to_string(),
            ]
        );
        let entries = loaded.hooks().pre_tool_use_entries();
        assert_eq!(entries[0], RuntimeHookCommand::new("legacy"));
        assert_eq!(entries[1].matcher(), Some("Bash"));
        assert!(entries[1].matches_tool("bash"));
        assert!(!entries[1].matches_tool("Read"));
        assert!(entries[3].matches_tool("ReadFile"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn records_object_style_hook_entries_without_command_441() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command"}]}]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load valid siblings and record malformed hook entry");

        assert!(loaded.hooks().pre_tool_use().is_empty());
        assert_eq!(loaded.hooks().invalid_count(), 1);
        assert_eq!(
            loaded.hooks().invalid_hooks()[0].kind,
            "invalid_hooks_config"
        );
        assert_eq!(loaded.hooks().invalid_hooks()[0].event, "PreToolUse");
        assert_eq!(loaded.hooks().invalid_hooks()[0].error_field, "command");
        assert!(loaded.hooks().invalid_hooks()[0]
            .reason
            .contains("command must be a non-empty string"));
        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn inspect_classifies_missing_loaded_and_legacy_skipped_files() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");
        fs::write(cwd.join(".claw.json"), "{not json").expect("write legacy config");
        fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"model":"opus"}"#,
        )
        .expect("write project settings");

        let inspection = ConfigLoader::new(&cwd, &home).inspect_collecting_warnings();

        assert!(
            inspection.load_error.is_none(),
            "{:?}",
            inspection.load_error
        );
        assert!(inspection.runtime_config.is_some());
        let loaded = inspection
            .files
            .iter()
            .find(|file| file.status == ConfigFileStatus::Loaded)
            .expect("loaded file");
        assert!(loaded.loaded);
        assert!(loaded.reason.is_none());
        let missing = inspection
            .files
            .iter()
            .find(|file| file.status == ConfigFileStatus::NotFound)
            .expect("missing file");
        assert_eq!(missing.reason.as_deref(), Some("not_found"));
        let skipped = inspection
            .files
            .iter()
            .find(|file| file.status == ConfigFileStatus::Skipped)
            .expect("skipped legacy file");
        assert_eq!(skipped.reason.as_deref(), Some("legacy_invalid_json"));
        assert!(!skipped.loaded);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn inspect_reports_parse_errors_but_keeps_valid_merged_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");
        fs::write(home.join("settings.json"), r#"{"model":"sonnet"}"#)
            .expect("write user settings");
        fs::write(cwd.join(".claw").join("settings.json"), "{not json")
            .expect("write invalid project settings");

        let inspection = ConfigLoader::new(&cwd, &home).inspect_collecting_warnings();

        assert!(inspection
            .load_error
            .as_deref()
            .is_some_and(|error| error.contains("settings.json")));
        let runtime_config = inspection.runtime_config.expect("valid files still merge");
        assert_eq!(runtime_config.model(), Some("sonnet"));
        let error_file = inspection
            .files
            .iter()
            .find(|file| file.status == ConfigFileStatus::LoadError)
            .expect("load error file");
        assert_eq!(error_file.reason.as_deref(), Some("parse_error"));
        assert!(error_file
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("settings.json")));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_sandbox_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{
              "sandbox": {
                "enabled": true,
                "namespaceRestrictions": false,
                "networkIsolation": true,
                "filesystemMode": "allow-list",
                "allowedMounts": ["logs", "tmp/cache"]
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(loaded.sandbox().enabled, Some(true));
        assert_eq!(loaded.sandbox().namespace_restrictions, Some(false));
        assert_eq!(loaded.sandbox().network_isolation, Some(true));
        assert_eq!(
            loaded.sandbox().filesystem_mode,
            Some(FilesystemIsolationMode::AllowList)
        );
        assert_eq!(loaded.sandbox().allowed_mounts, vec!["logs", "tmp/cache"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_provider_fallbacks_chain_with_primary_and_ordered_fallbacks() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");
        fs::write(
            home.join("settings.json"),
            r#"{
              "providerFallbacks": {
                "primary": "claude-opus-4-6",
                "fallbacks": ["grok-3", "grok-3-mini"]
              }
            }"#,
        )
        .expect("write provider fallback settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let chain = loaded.provider_fallbacks();
        assert_eq!(chain.primary(), Some("claude-opus-4-6"));
        assert_eq!(
            chain.fallbacks(),
            &["grok-3".to_string(), "grok-3-mini".to_string()]
        );
        assert!(!chain.is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn provider_fallbacks_default_is_empty_when_unset() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let chain = loaded.provider_fallbacks();
        assert_eq!(chain.primary(), None);
        assert!(chain.fallbacks().is_empty());
        assert!(chain.is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_rules_import_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"rulesImport": ["cursor", "copilot"]}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(loaded.rules_import().should_import("cursor"));
        assert!(loaded.rules_import().should_import("copilot"));
        assert!(!loaded.rules_import().should_import("windsurf"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn rules_import_none_disables_external_frameworks() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), r#"{"rulesImport": "none"}"#)
            .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(!loaded.rules_import().should_import("cursor"));
        assert!(!loaded.rules_import().should_import("copilot"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn rejects_rules_import_array_with_non_string_entries() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"rulesImport": ["cursor", 42]}"#,
        )
        .expect("write settings");

        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        assert!(error.to_string().contains("rulesImport"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_trusted_roots_from_settings() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"trustedRoots": ["/tmp/worktrees", "/home/user/projects"]}"#,
        )
        .expect("write settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let roots = loaded.trusted_roots();
        assert_eq!(roots, ["/tmp/worktrees", "/home/user/projects"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn trusted_roots_with_overrides_preserves_config_defaults_and_adds_per_call_roots() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"trustedRoots": ["/tmp/config-default", "/tmp/shared"]}"#,
        )
        .expect("write settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        let merged = loaded.trusted_roots_with_overrides(&[
            "/tmp/per-call".to_string(),
            "/tmp/shared".to_string(),
        ]);

        // then
        assert_eq!(
            merged,
            ["/tmp/config-default", "/tmp/shared", "/tmp/per-call"]
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn runtime_feature_trusted_roots_with_overrides_matches_runtime_config_merge() {
        let config = RuntimeFeatureConfig {
            trusted_roots: vec!["/tmp/config".to_string()],
            ..RuntimeFeatureConfig::default()
        };

        assert_eq!(
            config.trusted_roots_with_overrides(&["/tmp/per-call".to_string()]),
            ["/tmp/config", "/tmp/per-call"]
        );
    }

    #[test]
    fn trusted_roots_default_is_empty_when_unset() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        assert!(loaded.trusted_roots().is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_typed_mcp_and_oauth_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "stdio-server": {
                  "command": "uvx",
                  "args": ["mcp-server"],
                  "env": {"TOKEN": "secret"},
                  "required": true
                },
                "remote-server": {
                  "type": "http",
                  "url": "https://example.test/mcp",
                  "headers": {"Authorization": "Bearer token"},
                  "headersHelper": "helper.sh",
                  "oauth": {
                    "clientId": "mcp-client",
                    "callbackPort": 7777,
                    "authServerMetadataUrl": "https://issuer.test/.well-known/oauth-authorization-server",
                    "xaa": true
                  }
                }
              },
              "oauth": {
                "clientId": "runtime-client",
                "authorizeUrl": "https://console.test/oauth/authorize",
                "tokenUrl": "https://console.test/oauth/token",
                "callbackPort": 54545,
                "manualRedirectUrl": "https://console.test/oauth/callback",
                "scopes": ["org:read", "user:write"]
              }
            }"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{
              "mcpServers": {
                "remote-server": {
                  "type": "ws",
                  "url": "wss://override.test/mcp",
                  "headers": {"X-Env": "local"}
                }
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        let stdio_server = loaded
            .mcp()
            .get("stdio-server")
            .expect("stdio server should exist");
        assert_eq!(stdio_server.scope, ConfigSource::User);
        assert!(stdio_server.required);
        assert_eq!(stdio_server.transport(), McpTransport::Stdio);

        let remote_server = loaded
            .mcp()
            .get("remote-server")
            .expect("remote server should exist");
        assert_eq!(remote_server.scope, ConfigSource::Local);
        assert!(!remote_server.required);
        assert_eq!(remote_server.transport(), McpTransport::Ws);
        match &remote_server.config {
            McpServerConfig::Ws(config) => {
                assert_eq!(config.url, "wss://override.test/mcp");
                assert_eq!(
                    config.headers.get("X-Env").map(String::as_str),
                    Some("local")
                );
            }
            other => panic!("expected ws config, got {other:?}"),
        }

        let oauth = loaded.oauth().expect("oauth config should exist");
        assert_eq!(oauth.client_id, "runtime-client");
        assert_eq!(oauth.callback_port, Some(54_545));
        assert_eq!(oauth.scopes, vec!["org:read", "user:write"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn infers_http_mcp_servers_from_url_only_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "remote": {
                  "url": "https://example.test/mcp"
                }
              }
            }"#,
        )
        .expect("write mcp settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        let remote_server = loaded
            .mcp()
            .get("remote")
            .expect("remote server should exist");
        assert_eq!(remote_server.transport(), McpTransport::Http);
        match &remote_server.config {
            McpServerConfig::Http(config) => {
                assert_eq!(config.url, "https://example.test/mcp");
            }
            other => panic!("expected http config, got {other:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config_from_enabled_plugins() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "tool-guard@builtin": true,
                "sample-plugin@external": false
              }
            }"#,
        )
        .expect("write user settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.plugins().enabled_plugins().get("tool-guard@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("sample-plugin@external"),
            Some(&false)
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "core-helpers@builtin": true
              },
              "plugins": {
                "externalDirectories": ["./external-plugins"],
                "installRoot": "plugin-cache/installed",
                "registryPath": "plugin-cache/installed.json",
                "bundledRoot": "./bundled-plugins"
              }
            }"#,
        )
        .expect("write plugin settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("core-helpers@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded.plugins().external_directories(),
            &["./external-plugins".to_string()]
        );
        assert_eq!(
            loaded.plugins().install_root(),
            Some("plugin-cache/installed")
        );
        assert_eq!(
            loaded.plugins().registry_path(),
            Some("plugin-cache/installed.json")
        );
        assert_eq!(loaded.plugins().bundled_root(), Some("./bundled-plugins"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn records_invalid_mcp_server_shapes_without_rejecting_config_440() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"mcpServers":{"broken":{"type":"http","url":123}}}"#,
        )
        .expect("write broken settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("invalid MCP entries should not block otherwise loadable config");

        // then
        assert!(loaded.mcp().servers().is_empty());
        assert_eq!(loaded.mcp().total_configured(), 1);
        assert_eq!(loaded.mcp().invalid_count(), 1);
        let invalid = &loaded.mcp().invalid_servers()[0];
        assert_eq!(invalid.name, "broken");
        assert_eq!(invalid.error_field, "url");
        assert!(invalid
            .reason
            .contains("mcpServers.broken: missing string field url"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn loads_valid_mcp_servers_and_collects_all_invalid_siblings_440() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "valid-server": {"command": "/bin/echo", "args": ["hello"]},
                "missing-command": {"args": ["arg-only"]},
                "empty-command": {"command": ""},
                "wrong-type-command": {"command": 42},
                "extra-unknown-field": {"command": "/bin/echo", "extra": true}
              }
            }"#,
        )
        .expect("write mixed settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("valid MCP entries should load beside invalid siblings");

        assert_eq!(loaded.mcp().total_configured(), 5);
        assert_eq!(loaded.mcp().valid_count(), 1);
        assert_eq!(loaded.mcp().invalid_count(), 4);
        assert!(loaded.mcp().get("valid-server").is_some());
        let invalid_names = loaded
            .mcp()
            .invalid_servers()
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            invalid_names,
            vec![
                "empty-command",
                "extra-unknown-field",
                "missing-command",
                "wrong-type-command",
            ]
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_user_defined_model_aliases_from_settings() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{"aliases":{"fast":"claude-haiku-4-5-20251213","smart":"claude-opus-4-6"}}"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"aliases":{"smart":"claude-sonnet-4-6","cheap":"grok-3-mini"}}"#,
        )
        .expect("write local settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let aliases = loaded.aliases();
        assert_eq!(
            aliases.get("fast").map(String::as_str),
            Some("claude-haiku-4-5-20251213")
        );
        assert_eq!(
            aliases.get("smart").map(String::as_str),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            aliases.get("cheap").map(String::as_str),
            Some("grok-3-mini")
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn empty_settings_file_loads_defaults() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("empty settings should still load");

        // then
        assert_eq!(loaded.loaded_entries().len(), 1);
        assert_eq!(loaded.permission_mode(), None);
        assert_eq!(loaded.plugins().enabled_plugins().len(), 0);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn deep_merge_objects_merges_nested_maps() {
        // given
        let mut target = JsonValue::parse(r#"{"env":{"A":"1","B":"2"},"model":"haiku"}"#)
            .expect("target JSON should parse")
            .as_object()
            .expect("target should be an object")
            .clone();
        let source =
            JsonValue::parse(r#"{"env":{"B":"override","C":"3"},"sandbox":{"enabled":true}}"#)
                .expect("source JSON should parse")
                .as_object()
                .expect("source should be an object")
                .clone();

        // when
        deep_merge_objects(&mut target, &source);

        // then
        let env = target
            .get("env")
            .and_then(JsonValue::as_object)
            .expect("env should remain an object");
        assert_eq!(env.get("A"), Some(&JsonValue::String("1".to_string())));
        assert_eq!(
            env.get("B"),
            Some(&JsonValue::String("override".to_string()))
        );
        assert_eq!(env.get("C"), Some(&JsonValue::String("3".to_string())));
        assert!(target.contains_key("sandbox"));
    }

    #[test]
    fn loads_valid_hook_entries_and_records_invalid_siblings_441() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let project_settings = cwd.join(".claw").join("settings.json");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":["base"]}}"#,
        )
        .expect("write user settings");
        fs::write(
            &project_settings,
            r#"{"hooks":{"PreToolUse":["project",42]}}"#,
        )
        .expect("write invalid project settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load valid hook entries and record invalid siblings");

        // #106: arrays now concatenate across config layers, so both "base" and "project" are present
        assert_eq!(
            loaded.hooks().pre_tool_use(),
            &["base".to_string(), "project".to_string()]
        );
        assert_eq!(loaded.hooks().invalid_count(), 1);
        assert_eq!(loaded.hooks().invalid_hooks()[0].event, "PreToolUse");
        assert_eq!(
            loaded.hooks().invalid_hooks()[0].kind,
            "invalid_hooks_config"
        );
        // #106: invalid entry at index 2 after array concatenation
        assert_eq!(loaded.hooks().invalid_hooks()[0].index, Some(2));
        assert!(loaded.hooks().invalid_hooks()[0]
            .reason
            .contains("must be a string or hook object"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn permission_mode_aliases_resolve_to_expected_modes() {
        // given / when / then
        assert_eq!(
            parse_permission_mode_label("plan", "test").expect("plan should resolve"),
            ResolvedPermissionMode::ReadOnly
        );
        assert_eq!(
            parse_permission_mode_label("acceptEdits", "test").expect("acceptEdits should resolve"),
            ResolvedPermissionMode::WorkspaceWrite
        );
        assert_eq!(
            parse_permission_mode_label("dontAsk", "test").expect("dontAsk should resolve"),
            ResolvedPermissionMode::DangerFullAccess
        );
    }

    #[test]
    fn hook_config_merge_preserves_uniques() {
        // given
        let base = RuntimeHookConfig::new(
            vec!["pre-a".to_string()],
            vec!["post-a".to_string()],
            vec!["failure-a".to_string()],
        );
        let overlay = RuntimeHookConfig::new(
            vec!["pre-a".to_string(), "pre-b".to_string()],
            vec!["post-a".to_string(), "post-b".to_string()],
            vec!["failure-b".to_string()],
        );

        // when
        let merged = base.merged(&overlay);

        // then
        assert_eq!(
            merged.pre_tool_use(),
            &["pre-a".to_string(), "pre-b".to_string()]
        );
        assert_eq!(
            merged.post_tool_use(),
            &["post-a".to_string(), "post-b".to_string()]
        );
        assert_eq!(
            merged.post_tool_use_failure(),
            &["failure-a".to_string(), "failure-b".to_string()]
        );
    }

    #[test]
    fn plugin_state_falls_back_to_default_for_unknown_plugin() {
        // given
        let mut config = RuntimePluginConfig::default();
        config.set_plugin_state("known".to_string(), true);

        // when / then
        assert!(config.state_for("known", false));
        assert!(config.state_for("missing", true));
        assert!(!config.state_for("missing", false));
    }

    #[test]
    fn validates_unknown_top_level_keys_with_line_and_field_name() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"model\": \"opus\",\n  \"telemetry\": true\n}\n",
        )
        .expect("write user settings");

        // when
        let (_config, warnings) = ConfigLoader::new(&cwd, &home)
            .load_collecting_warnings()
            .expect("unknown config keys should load with warnings");

        // then
        let rendered = warnings.join("\n");
        assert!(
            rendered.contains(&user_settings.display().to_string()),
            "warning should include file path, got: {rendered}"
        );
        assert!(
            rendered.contains("line 3"),
            "warning should include line number, got: {rendered}"
        );
        assert!(
            rendered.contains("telemetry"),
            "warning should name the offending field, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn validates_deprecated_top_level_keys_with_replacement_guidance() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"model\": \"opus\",\n  \"allowedTools\": [\"Read\"]\n}\n",
        )
        .expect("write user settings");

        // when
        let (_config, warnings) = ConfigLoader::new(&cwd, &home)
            .load_collecting_warnings()
            .expect("legacy unknown config keys should load with warnings");

        // then
        let rendered = warnings.join("\n");
        assert!(
            rendered.contains(&user_settings.display().to_string()),
            "warning should include file path, got: {rendered}"
        );
        assert!(
            rendered.contains("line 3"),
            "warning should include line number, got: {rendered}"
        );
        assert!(
            rendered.contains("allowedTools"),
            "warning should name the offending field, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn hook_event_wrong_type_is_recorded_without_config_failure_441() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"hooks\": {\n    \"PreToolUse\": \"not-an-array\"\n  }\n}\n",
        )
        .expect("write user settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should record malformed hook event without failing");

        assert!(loaded.hooks().pre_tool_use().is_empty());
        assert_eq!(loaded.hooks().invalid_count(), 1);
        assert_eq!(loaded.hooks().invalid_hooks()[0].event, "PreToolUse");
        assert_eq!(
            loaded.hooks().invalid_hooks()[0].kind,
            "invalid_hooks_config"
        );
        assert_eq!(loaded.hooks().invalid_hooks()[0].index, None);
        assert!(loaded.hooks().invalid_hooks()[0]
            .reason
            .contains("field PreToolUse must be an array"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn collects_all_invalid_hook_siblings_instead_of_halting_at_first_441() {
        // ROADMAP #441 finding (c): first-error-only halting means users must fix
        // one hook at a time. After #441 partial fix, all invalid entries in the
        // same config are collected.
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":[42],"PostToolUse":"not-an-array","InvalidEvent":["cmd"]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should collect all invalid hooks without halting at first");

        assert!(loaded.hooks().pre_tool_use().is_empty());
        assert!(loaded.hooks().post_tool_use().is_empty());
        // Three distinct invalid entries: 42, wrong type, unknown event
        assert_eq!(loaded.hooks().invalid_count(), 3);

        let invalid = loaded.hooks().invalid_hooks();
        // PreToolUse[0]=42
        assert_eq!(invalid[0].event, "PreToolUse");
        assert_eq!(invalid[0].index, Some(0));
        assert_eq!(invalid[0].kind, "invalid_hooks_config");
        // PostToolUse wrong type
        assert_eq!(invalid[1].event, "PostToolUse");
        assert_eq!(invalid[1].index, None);
        assert_eq!(invalid[1].kind, "invalid_hooks_config");
        // Unknown event
        assert_eq!(invalid[2].event, "InvalidEvent");
        assert_eq!(invalid[2].index, None);
        assert_eq!(invalid[2].kind, "unknown_hook_event");
        assert!(invalid[2]
            .reason
            .contains("unknown hook event InvalidEvent"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn unknown_hook_events_recorded_with_correct_kind_441() {
        // ROADMAP #441 finding (a): unknown event names like Stop/Notification
        // should not reject entire hooks config; they are recorded as invalid.
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":["valid-cmd"],"Stop":"not-an-array","Notification":[{}]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load valid hooks and record unknown event siblings");

        // Valid PreToolUse hook should load
        assert_eq!(loaded.hooks().pre_tool_use(), &["valid-cmd".to_string()]);
        // Stop and Notification are unknown events; each gets one invalid entry
        // Notification:[{}] also has an empty-object entry issue but since we
        // don't parse unknown events, only the unknown-event invalid is recorded
        let invalid = loaded.hooks().invalid_hooks();
        assert!(
            invalid.len() >= 2,
            "expected at least 2 invalid hooks, got {}",
            invalid.len()
        );

        let stop = invalid
            .iter()
            .find(|h| h.event == "Stop")
            .expect("Stop invalid hook");
        assert_eq!(stop.kind, "unknown_hook_event");
        assert_eq!(stop.index, None);
        assert!(stop.reason.contains("unknown hook event Stop"));

        let notif = invalid
            .iter()
            .find(|h| h.event == "Notification")
            .expect("Notification invalid hook");
        assert_eq!(notif.kind, "unknown_hook_event");

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn documented_claude_code_hook_format_loads_without_error_441() {
        // ROADMAP #441: the Claude Code documented hook format
        // {"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"..."}]}]}}
        // must load without config_load_error.
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"/bin/echo pretool"}]}]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("Claude Code documented hook format must load without error");

        assert_eq!(
            loaded.hooks().pre_tool_use(),
            &["/bin/echo pretool".to_string()]
        );
        assert_eq!(loaded.hooks().invalid_count(), 0);
        let entries = loaded.hooks().pre_tool_use_entries();
        assert_eq!(entries[0].matcher(), Some("Read"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn unknown_top_level_key_suggests_closest_match() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(&user_settings, "{\n  \"modle\": \"opus\"\n}\n").expect("write user settings");

        // when
        let (_config, warnings) = ConfigLoader::new(&cwd, &home)
            .load_collecting_warnings()
            .expect("unknown config keys should load with warnings");

        // then
        let rendered = warnings.join("\n");
        assert!(
            rendered.contains("modle"),
            "warning should name the offending field, got: {rendered}"
        );
        assert!(
            rendered.contains("model"),
            "warning should suggest the closest known key, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }
}
