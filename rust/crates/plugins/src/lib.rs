mod hooks;
#[cfg(test)]
pub mod test_isolation;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use hooks::{HookEvent, HookRunResult, HookRunner};

const EXTERNAL_MARKETPLACE: &str = "external";
const BUILTIN_MARKETPLACE: &str = "builtin";
const BUNDLED_MARKETPLACE: &str = "bundled";
const SETTINGS_FILE_NAME: &str = "settings.json";
const REGISTRY_FILE_NAME: &str = "installed.json";
const MANIFEST_FILE_NAME: &str = "plugin.json";
const MANIFEST_RELATIVE_PATH: &str = ".claude-plugin/plugin.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Builtin,
    Bundled,
    External,
}

impl Display for PluginKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin => write!(f, "builtin"),
            Self::Bundled => write!(f, "bundled"),
            Self::External => write!(f, "external"),
        }
    }
}

impl PluginKind {
    #[must_use]
    fn marketplace(self) -> &'static str {
        match self {
            Self::Builtin => BUILTIN_MARKETPLACE,
            Self::Bundled => BUNDLED_MARKETPLACE,
            Self::External => EXTERNAL_MARKETPLACE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub source: String,
    pub default_enabled: bool,
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHooks {
    #[serde(rename = "PreToolUse", default)]
    pub pre_tool_use: Vec<String>,
    #[serde(rename = "PostToolUse", default)]
    pub post_tool_use: Vec<String>,
    #[serde(rename = "PostToolUseFailure", default)]
    pub post_tool_use_failure: Vec<String>,
}

impl PluginHooks {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.post_tool_use_failure.is_empty()
    }

    #[must_use]
    pub fn merged_with(&self, other: &Self) -> Self {
        let mut merged = self.clone();
        merged
            .pre_tool_use
            .extend(other.pre_tool_use.iter().cloned());
        merged
            .post_tool_use
            .extend(other.post_tool_use.iter().cloned());
        merged
            .post_tool_use_failure
            .extend(other.post_tool_use_failure.iter().cloned());
        merged
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLifecycle {
    #[serde(rename = "Init", default)]
    pub init: Vec<String>,
    #[serde(rename = "Shutdown", default)]
    pub shutdown: Vec<String>,
}

impl PluginLifecycle {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.init.is_empty() && self.shutdown.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<PluginPermission>,
    #[serde(rename = "defaultEnabled", default)]
    pub default_enabled: bool,
    #[serde(default)]
    pub hooks: PluginHooks,
    #[serde(default)]
    pub lifecycle: PluginLifecycle,
    #[serde(default)]
    pub tools: Vec<PluginToolManifest>,
    #[serde(default)]
    pub commands: Vec<PluginCommandManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginPermission {
    Read,
    Write,
    Execute,
}

impl PluginPermission {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "execute" => Some(Self::Execute),
            _ => None,
        }
    }
}

impl AsRef<str> for PluginPermission {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginToolManifest {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub required_permission: PluginToolPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginToolPermission {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl PluginToolPermission {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCommandManifest {
    pub name: String,
    pub description: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawPluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(rename = "defaultEnabled", default)]
    pub default_enabled: bool,
    #[serde(default)]
    pub hooks: PluginHooks,
    #[serde(default)]
    pub lifecycle: PluginLifecycle,
    #[serde(default)]
    pub tools: Vec<RawPluginToolManifest>,
    #[serde(default)]
    pub commands: Vec<PluginCommandManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawPluginToolManifest {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(
        rename = "requiredPermission",
        default = "default_tool_permission_label"
    )]
    pub required_permission: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginTool {
    plugin_id: String,
    plugin_name: String,
    definition: PluginToolDefinition,
    command: String,
    args: Vec<String>,
    required_permission: PluginToolPermission,
    root: Option<PathBuf>,
}

impl PluginTool {
    #[must_use]
    pub fn new(
        plugin_id: impl Into<String>,
        plugin_name: impl Into<String>,
        definition: PluginToolDefinition,
        command: impl Into<String>,
        args: Vec<String>,
        required_permission: PluginToolPermission,
        root: Option<PathBuf>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            plugin_name: plugin_name.into(),
            definition,
            command: command.into(),
            args,
            required_permission,
            root,
        }
    }

    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    #[must_use]
    pub fn definition(&self) -> &PluginToolDefinition {
        &self.definition
    }

    #[must_use]
    pub fn required_permission(&self) -> &str {
        self.required_permission.as_str()
    }

    pub fn execute(&self, input: &Value) -> Result<String, PluginError> {
        let input_json = input.to_string();
        let mut process = Command::new(&self.command);
        process
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAWD_PLUGIN_ID", &self.plugin_id)
            .env("CLAWD_PLUGIN_NAME", &self.plugin_name)
            .env("CLAWD_TOOL_NAME", &self.definition.name)
            .env("CLAWD_TOOL_INPUT", &input_json);
        if let Some(root) = &self.root {
            process
                .current_dir(root)
                .env("CLAWD_PLUGIN_ROOT", root.display().to_string());
        }

        let mut child = process.spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write as _;
            stdin.write_all(input_json.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(PluginError::CommandFailed(format!(
                "plugin tool `{}` from `{}` failed for `{}`: {}",
                self.definition.name,
                self.plugin_id,
                self.command,
                if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                }
            )))
        }
    }
}

fn default_tool_permission_label() -> String {
    "danger-full-access".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginInstallSource {
    LocalPath { path: PathBuf },
    GitUrl { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPluginRecord {
    #[serde(default = "default_plugin_kind")]
    pub kind: PluginKind,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub install_path: PathBuf,
    pub source: PluginInstallSource,
    pub installed_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPluginRegistry {
    #[serde(default)]
    pub plugins: BTreeMap<String, InstalledPluginRecord>,
}

fn default_plugin_kind() -> PluginKind {
    PluginKind::External
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinPlugin {
    metadata: PluginMetadata,
    hooks: PluginHooks,
    lifecycle: PluginLifecycle,
    tools: Vec<PluginTool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BundledPlugin {
    metadata: PluginMetadata,
    hooks: PluginHooks,
    lifecycle: PluginLifecycle,
    tools: Vec<PluginTool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalPlugin {
    metadata: PluginMetadata,
    hooks: PluginHooks,
    lifecycle: PluginLifecycle,
    tools: Vec<PluginTool>,
}

pub trait Plugin {
    fn metadata(&self) -> &PluginMetadata;
    fn hooks(&self) -> &PluginHooks;
    fn lifecycle(&self) -> &PluginLifecycle;
    fn tools(&self) -> &[PluginTool];
    fn validate(&self) -> Result<(), PluginError>;
    fn initialize(&self) -> Result<(), PluginError>;
    fn shutdown(&self) -> Result<(), PluginError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginDefinition {
    Builtin(BuiltinPlugin),
    Bundled(BundledPlugin),
    External(ExternalPlugin),
}

impl Plugin for BuiltinPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    fn hooks(&self) -> &PluginHooks {
        &self.hooks
    }

    fn lifecycle(&self) -> &PluginLifecycle {
        &self.lifecycle
    }

    fn tools(&self) -> &[PluginTool] {
        &self.tools
    }

    fn validate(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn initialize(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Plugin for BundledPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    fn hooks(&self) -> &PluginHooks {
        &self.hooks
    }

    fn lifecycle(&self) -> &PluginLifecycle {
        &self.lifecycle
    }

    fn tools(&self) -> &[PluginTool] {
        &self.tools
    }

    fn validate(&self) -> Result<(), PluginError> {
        validate_hook_paths(self.metadata.root.as_deref(), &self.hooks)?;
        validate_lifecycle_paths(self.metadata.root.as_deref(), &self.lifecycle)?;
        validate_tool_paths(self.metadata.root.as_deref(), &self.tools)
    }

    fn initialize(&self) -> Result<(), PluginError> {
        run_lifecycle_commands(
            self.metadata(),
            self.lifecycle(),
            "init",
            &self.lifecycle.init,
        )
    }

    fn shutdown(&self) -> Result<(), PluginError> {
        run_lifecycle_commands(
            self.metadata(),
            self.lifecycle(),
            "shutdown",
            &self.lifecycle.shutdown,
        )
    }
}

impl Plugin for ExternalPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    fn hooks(&self) -> &PluginHooks {
        &self.hooks
    }

    fn lifecycle(&self) -> &PluginLifecycle {
        &self.lifecycle
    }

    fn tools(&self) -> &[PluginTool] {
        &self.tools
    }

    fn validate(&self) -> Result<(), PluginError> {
        validate_hook_paths(self.metadata.root.as_deref(), &self.hooks)?;
        validate_lifecycle_paths(self.metadata.root.as_deref(), &self.lifecycle)?;
        validate_tool_paths(self.metadata.root.as_deref(), &self.tools)
    }

    fn initialize(&self) -> Result<(), PluginError> {
        run_lifecycle_commands(
            self.metadata(),
            self.lifecycle(),
            "init",
            &self.lifecycle.init,
        )
    }

    fn shutdown(&self) -> Result<(), PluginError> {
        run_lifecycle_commands(
            self.metadata(),
            self.lifecycle(),
            "shutdown",
            &self.lifecycle.shutdown,
        )
    }
}

impl Plugin for PluginDefinition {
    fn metadata(&self) -> &PluginMetadata {
        match self {
            Self::Builtin(plugin) => plugin.metadata(),
            Self::Bundled(plugin) => plugin.metadata(),
            Self::External(plugin) => plugin.metadata(),
        }
    }

    fn hooks(&self) -> &PluginHooks {
        match self {
            Self::Builtin(plugin) => plugin.hooks(),
            Self::Bundled(plugin) => plugin.hooks(),
            Self::External(plugin) => plugin.hooks(),
        }
    }

    fn lifecycle(&self) -> &PluginLifecycle {
        match self {
            Self::Builtin(plugin) => plugin.lifecycle(),
            Self::Bundled(plugin) => plugin.lifecycle(),
            Self::External(plugin) => plugin.lifecycle(),
        }
    }

    fn tools(&self) -> &[PluginTool] {
        match self {
            Self::Builtin(plugin) => plugin.tools(),
            Self::Bundled(plugin) => plugin.tools(),
            Self::External(plugin) => plugin.tools(),
        }
    }

    fn validate(&self) -> Result<(), PluginError> {
        match self {
            Self::Builtin(plugin) => plugin.validate(),
            Self::Bundled(plugin) => plugin.validate(),
            Self::External(plugin) => plugin.validate(),
        }
    }

    fn initialize(&self) -> Result<(), PluginError> {
        match self {
            Self::Builtin(plugin) => plugin.initialize(),
            Self::Bundled(plugin) => plugin.initialize(),
            Self::External(plugin) => plugin.initialize(),
        }
    }

    fn shutdown(&self) -> Result<(), PluginError> {
        match self {
            Self::Builtin(plugin) => plugin.shutdown(),
            Self::Bundled(plugin) => plugin.shutdown(),
            Self::External(plugin) => plugin.shutdown(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredPlugin {
    definition: PluginDefinition,
    enabled: bool,
}

impl RegisteredPlugin {
    #[must_use]
    pub fn new(definition: PluginDefinition, enabled: bool) -> Self {
        Self {
            definition,
            enabled,
        }
    }

    #[must_use]
    pub fn metadata(&self) -> &PluginMetadata {
        self.definition.metadata()
    }

    #[must_use]
    pub fn hooks(&self) -> &PluginHooks {
        self.definition.hooks()
    }

    #[must_use]
    pub fn tools(&self) -> &[PluginTool] {
        self.definition.tools()
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        self.definition.validate()
    }

    pub fn initialize(&self) -> Result<(), PluginError> {
        self.definition.initialize()
    }

    pub fn shutdown(&self) -> Result<(), PluginError> {
        self.definition.shutdown()
    }

    #[must_use]
    pub fn summary(&self) -> PluginSummary {
        PluginSummary {
            metadata: self.metadata().clone(),
            enabled: self.enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub metadata: PluginMetadata,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct PluginLoadFailure {
    pub plugin_root: PathBuf,
    pub kind: PluginKind,
    pub source: String,
    error: Box<PluginError>,
}

impl PluginLoadFailure {
    #[must_use]
    pub fn new(plugin_root: PathBuf, kind: PluginKind, source: String, error: PluginError) -> Self {
        Self {
            plugin_root,
            kind,
            source,
            error: Box::new(error),
        }
    }

    #[must_use]
    pub fn error(&self) -> &PluginError {
        self.error.as_ref()
    }
}

impl Display for PluginLoadFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to load {} plugin from `{}` (source: {}): {}",
            self.kind,
            self.plugin_root.display(),
            self.source,
            self.error()
        )
    }
}

#[derive(Debug)]
pub struct PluginRegistryReport {
    registry: PluginRegistry,
    failures: Vec<PluginLoadFailure>,
}

impl PluginRegistryReport {
    #[must_use]
    pub fn new(registry: PluginRegistry, failures: Vec<PluginLoadFailure>) -> Self {
        Self { registry, failures }
    }

    #[must_use]
    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    #[must_use]
    pub fn failures(&self) -> &[PluginLoadFailure] {
        &self.failures
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    #[must_use]
    pub fn summaries(&self) -> Vec<PluginSummary> {
        self.registry.summaries()
    }

    pub fn into_registry(self) -> Result<PluginRegistry, PluginError> {
        if self.failures.is_empty() {
            Ok(self.registry)
        } else {
            Err(PluginError::LoadFailures(self.failures))
        }
    }
}

#[derive(Debug, Default)]
struct PluginDiscovery {
    plugins: Vec<PluginDefinition>,
    failures: Vec<PluginLoadFailure>,
}

impl PluginDiscovery {
    fn push_plugin(&mut self, plugin: PluginDefinition) {
        self.plugins.push(plugin);
    }

    fn push_failure(&mut self, failure: PluginLoadFailure) {
        self.failures.push(failure);
    }

    fn extend(&mut self, other: Self) {
        self.plugins.extend(other.plugins);
        self.failures.extend(other.failures);
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginRegistry {
    plugins: Vec<RegisteredPlugin>,
}

impl PluginRegistry {
    #[must_use]
    pub fn new(mut plugins: Vec<RegisteredPlugin>) -> Self {
        plugins.sort_by(|left, right| left.metadata().id.cmp(&right.metadata().id));
        Self { plugins }
    }

    #[must_use]
    pub fn plugins(&self) -> &[RegisteredPlugin] {
        &self.plugins
    }

    #[must_use]
    pub fn get(&self, plugin_id: &str) -> Option<&RegisteredPlugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.metadata().id == plugin_id)
    }

    #[must_use]
    pub fn contains(&self, plugin_id: &str) -> bool {
        self.get(plugin_id).is_some()
    }

    #[must_use]
    pub fn summaries(&self) -> Vec<PluginSummary> {
        self.plugins.iter().map(RegisteredPlugin::summary).collect()
    }

    pub fn aggregated_hooks(&self) -> Result<PluginHooks, PluginError> {
        self.plugins
            .iter()
            .filter(|plugin| plugin.is_enabled())
            .try_fold(PluginHooks::default(), |acc, plugin| {
                plugin.validate()?;
                Ok(acc.merged_with(plugin.hooks()))
            })
    }

    pub fn aggregated_tools(&self) -> Result<Vec<PluginTool>, PluginError> {
        let mut tools = Vec::new();
        let mut seen_names = BTreeMap::new();
        for plugin in self.plugins.iter().filter(|plugin| plugin.is_enabled()) {
            plugin.validate()?;
            for tool in plugin.tools() {
                if let Some(existing_plugin) =
                    seen_names.insert(tool.definition().name.clone(), tool.plugin_id().to_string())
                {
                    return Err(PluginError::InvalidManifest(format!(
                        "plugin tool `{}` is defined by both `{existing_plugin}` and `{}`",
                        tool.definition().name,
                        tool.plugin_id()
                    )));
                }
                tools.push(tool.clone());
            }
        }
        Ok(tools)
    }

    pub fn initialize(&self) -> Result<(), PluginError> {
        for plugin in self.plugins.iter().filter(|plugin| plugin.is_enabled()) {
            plugin.validate()?;
            plugin.initialize()?;
        }
        Ok(())
    }

    pub fn shutdown(&self) -> Result<(), PluginError> {
        for plugin in self
            .plugins
            .iter()
            .rev()
            .filter(|plugin| plugin.is_enabled())
        {
            plugin.shutdown()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManagerConfig {
    pub config_home: PathBuf,
    pub enabled_plugins: BTreeMap<String, bool>,
    pub external_dirs: Vec<PathBuf>,
    pub install_root: Option<PathBuf>,
    pub registry_path: Option<PathBuf>,
    pub bundled_root: Option<PathBuf>,
}

impl PluginManagerConfig {
    #[must_use]
    pub fn new(config_home: impl Into<PathBuf>) -> Self {
        Self {
            config_home: config_home.into(),
            enabled_plugins: BTreeMap::new(),
            external_dirs: Vec::new(),
            install_root: None,
            registry_path: None,
            bundled_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManager {
    config: PluginManagerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub plugin_id: String,
    pub version: String,
    pub install_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub plugin_id: String,
    pub old_version: String,
    pub new_version: String,
    pub install_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginManifestValidationError {
    EmptyField {
        field: &'static str,
    },
    EmptyEntryField {
        kind: &'static str,
        field: &'static str,
        name: Option<String>,
    },
    InvalidPermission {
        permission: String,
    },
    DuplicatePermission {
        permission: String,
    },
    DuplicateEntry {
        kind: &'static str,
        name: String,
    },
    MissingPath {
        kind: &'static str,
        path: PathBuf,
    },
    PathIsDirectory {
        kind: &'static str,
        path: PathBuf,
    },
    InvalidToolInputSchema {
        tool_name: String,
    },
    InvalidToolRequiredPermission {
        tool_name: String,
        permission: String,
    },
    UnsupportedManifestContract {
        detail: String,
    },
}

impl Display for PluginManifestValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyField { field } => {
                write!(f, "plugin manifest {field} cannot be empty")
            }
            Self::EmptyEntryField { kind, field, name } => match name {
                Some(name) if !name.is_empty() => {
                    write!(f, "plugin {kind} `{name}` {field} cannot be empty")
                }
                _ => write!(f, "plugin {kind} {field} cannot be empty"),
            },
            Self::InvalidPermission { permission } => {
                write!(
                    f,
                    "plugin manifest permission `{permission}` must be one of read, write, or execute"
                )
            }
            Self::DuplicatePermission { permission } => {
                write!(f, "plugin manifest permission `{permission}` is duplicated")
            }
            Self::DuplicateEntry { kind, name } => {
                write!(f, "plugin {kind} `{name}` is duplicated")
            }
            Self::MissingPath { kind, path } => {
                write!(f, "{kind} path `{}` does not exist", path.display())
            }
            Self::PathIsDirectory { kind, path } => {
                write!(f, "{kind} path `{}` must point to a file", path.display())
            }
            Self::InvalidToolInputSchema { tool_name } => {
                write!(
                    f,
                    "plugin tool `{tool_name}` inputSchema must be a JSON object"
                )
            }
            Self::InvalidToolRequiredPermission {
                tool_name,
                permission,
            } => write!(
                f,
                "plugin tool `{tool_name}` requiredPermission `{permission}` must be read-only, workspace-write, or danger-full-access"
            ),
            Self::UnsupportedManifestContract { detail } => f.write_str(detail),
        }
    }
}

#[derive(Debug)]
pub enum PluginError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ManifestValidation(Vec<PluginManifestValidationError>),
    LoadFailures(Vec<PluginLoadFailure>),
    InvalidManifest(String),
    NotFound(String),
    CommandFailed(String),
}

impl Display for PluginError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::ManifestValidation(errors) => {
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{error}")?;
                }
                Ok(())
            }
            Self::LoadFailures(failures) => {
                for (index, failure) in failures.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{failure}")?;
                }
                Ok(())
            }
            Self::InvalidManifest(message)
            | Self::NotFound(message)
            | Self::CommandFailed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<std::io::Error> for PluginError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for PluginError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl PluginManager {
    #[must_use]
    pub fn new(config: PluginManagerConfig) -> Self {
        Self { config }
    }

    /// Returns the default bundled plugins root directory.
    ///
    /// Resolution order (first existing path wins):
    /// 1. `<exe_dir>/../share/claw/plugins/bundled` — standard install layout
    /// 2. `<exe_dir>/bundled` — simple relocated layout
    /// 3. `CARGO_MANIFEST_DIR/bundled` — dev/source-tree fallback (only if it exists)
    /// 4. `<exe_dir>/../share/claw/plugins/bundled` — canonical default even if missing
    ///
    /// This avoids baking in a compile-time source-tree path that may be
    /// inaccessible at runtime (e.g. a root-owned repo directory).
    #[must_use]
    pub fn bundled_root() -> PathBuf {
        // Candidate 1: standard FHS install layout — <prefix>/bin/claw -> <prefix>/share/claw/plugins/bundled
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let share_path = exe_dir
                    .join("..")
                    .join("share")
                    .join("claw")
                    .join("plugins")
                    .join("bundled");
                if share_path.exists() {
                    return share_path;
                }

                // Candidate 2: simple adjacent layout — <exe_dir>/bundled
                let adjacent = exe_dir.join("bundled");
                if adjacent.exists() {
                    return adjacent;
                }
            }
        }

        // Candidate 3: dev/source-tree fallback — only if the directory actually exists
        let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled");
        if dev_path.exists() {
            return dev_path;
        }

        // Default (nothing found): return the canonical install path even if missing,
        // so callers get an empty plugin list rather than a permission error.
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                return exe_dir
                    .join("..")
                    .join("share")
                    .join("claw")
                    .join("plugins")
                    .join("bundled");
            }
        }

        // Last resort fallback
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled")
    }

    #[must_use]
    pub fn install_root(&self) -> PathBuf {
        self.config
            .install_root
            .clone()
            .unwrap_or_else(|| self.config.config_home.join("plugins").join("installed"))
    }

    #[must_use]
    pub fn registry_path(&self) -> PathBuf {
        self.config.registry_path.clone().unwrap_or_else(|| {
            self.config
                .config_home
                .join("plugins")
                .join(REGISTRY_FILE_NAME)
        })
    }

    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.config.config_home.join(SETTINGS_FILE_NAME)
    }

    pub fn plugin_registry(&self) -> Result<PluginRegistry, PluginError> {
        self.plugin_registry_report()?.into_registry()
    }

    pub fn plugin_registry_report(&self) -> Result<PluginRegistryReport, PluginError> {
        self.sync_bundled_plugins()?;

        let mut discovery = PluginDiscovery::default();
        discovery.plugins.extend(builtin_plugins());

        let installed = self.discover_installed_plugins_with_failures()?;
        discovery.extend(installed);

        let external =
            self.discover_external_directory_plugins_with_failures(&discovery.plugins)?;
        discovery.extend(external);

        Ok(self.build_registry_report(discovery))
    }

    pub fn list_plugins(&self) -> Result<Vec<PluginSummary>, PluginError> {
        Ok(self.plugin_registry()?.summaries())
    }

    pub fn list_installed_plugins(&self) -> Result<Vec<PluginSummary>, PluginError> {
        Ok(self.installed_plugin_registry()?.summaries())
    }

    pub fn discover_plugins(&self) -> Result<Vec<PluginDefinition>, PluginError> {
        Ok(self
            .plugin_registry()?
            .plugins
            .into_iter()
            .map(|plugin| plugin.definition)
            .collect())
    }

    pub fn aggregated_hooks(&self) -> Result<PluginHooks, PluginError> {
        self.plugin_registry()?.aggregated_hooks()
    }

    pub fn aggregated_tools(&self) -> Result<Vec<PluginTool>, PluginError> {
        self.plugin_registry()?.aggregated_tools()
    }

    pub fn validate_plugin_source(&self, source: &str) -> Result<PluginManifest, PluginError> {
        let path = resolve_local_source(source)?;
        load_plugin_from_directory(&path)
    }

    pub fn install(&mut self, source: &str) -> Result<InstallOutcome, PluginError> {
        let install_source = parse_install_source(source)?;
        let temp_root = self.install_root().join(".tmp");
        let staged_source = materialize_source(&install_source, &temp_root)?;
        let cleanup_source = matches!(install_source, PluginInstallSource::GitUrl { .. });
        let manifest = load_plugin_from_directory(&staged_source)?;

        let plugin_id = plugin_id(&manifest.name, EXTERNAL_MARKETPLACE);
        let install_path = self.install_root().join(sanitize_plugin_id(&plugin_id));
        if install_path.exists() {
            fs::remove_dir_all(&install_path)?;
        }
        copy_dir_all(&staged_source, &install_path)?;
        if cleanup_source {
            let _ = fs::remove_dir_all(&staged_source);
        }

        let now = unix_time_ms();
        let record = InstalledPluginRecord {
            kind: PluginKind::External,
            id: plugin_id.clone(),
            name: manifest.name,
            version: manifest.version.clone(),
            description: manifest.description,
            install_path: install_path.clone(),
            source: install_source,
            installed_at_unix_ms: now,
            updated_at_unix_ms: now,
        };

        let mut registry = self.load_registry()?;
        registry.plugins.insert(plugin_id.clone(), record);
        self.store_registry(&registry)?;
        self.write_enabled_state(&plugin_id, Some(true))?;
        self.config.enabled_plugins.insert(plugin_id.clone(), true);

        Ok(InstallOutcome {
            plugin_id,
            version: manifest.version,
            install_path,
        })
    }

    pub fn enable(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        self.ensure_known_plugin(plugin_id)?;
        self.write_enabled_state(plugin_id, Some(true))?;
        self.config
            .enabled_plugins
            .insert(plugin_id.to_string(), true);
        Ok(())
    }

    pub fn disable(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        self.ensure_known_plugin(plugin_id)?;
        self.write_enabled_state(plugin_id, Some(false))?;
        self.config
            .enabled_plugins
            .insert(plugin_id.to_string(), false);
        Ok(())
    }

    pub fn uninstall(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        let mut registry = self.load_registry()?;
        let record = registry.plugins.remove(plugin_id).ok_or_else(|| {
            PluginError::NotFound(format!("plugin `{plugin_id}` is not installed"))
        })?;
        if record.kind == PluginKind::Bundled {
            registry.plugins.insert(plugin_id.to_string(), record);
            return Err(PluginError::CommandFailed(format!(
                "plugin `{plugin_id}` is bundled and managed automatically; disable it instead"
            )));
        }
        if record.install_path.exists() {
            fs::remove_dir_all(&record.install_path)?;
        }
        self.store_registry(&registry)?;
        self.write_enabled_state(plugin_id, None)?;
        self.config.enabled_plugins.remove(plugin_id);
        Ok(())
    }

    pub fn update(&mut self, plugin_id: &str) -> Result<UpdateOutcome, PluginError> {
        let mut registry = self.load_registry()?;
        let record = registry.plugins.get(plugin_id).cloned().ok_or_else(|| {
            PluginError::NotFound(format!("plugin `{plugin_id}` is not installed"))
        })?;

        let temp_root = self.install_root().join(".tmp");
        let staged_source = materialize_source(&record.source, &temp_root)?;
        let cleanup_source = matches!(record.source, PluginInstallSource::GitUrl { .. });
        let manifest = load_plugin_from_directory(&staged_source)?;

        if record.install_path.exists() {
            fs::remove_dir_all(&record.install_path)?;
        }
        copy_dir_all(&staged_source, &record.install_path)?;
        if cleanup_source {
            let _ = fs::remove_dir_all(&staged_source);
        }

        let updated_record = InstalledPluginRecord {
            version: manifest.version.clone(),
            description: manifest.description,
            updated_at_unix_ms: unix_time_ms(),
            ..record.clone()
        };
        registry
            .plugins
            .insert(plugin_id.to_string(), updated_record);
        self.store_registry(&registry)?;

        Ok(UpdateOutcome {
            plugin_id: plugin_id.to_string(),
            old_version: record.version,
            new_version: manifest.version,
            install_path: record.install_path,
        })
    }

    fn discover_installed_plugins_with_failures(&self) -> Result<PluginDiscovery, PluginError> {
        let mut registry = self.load_registry()?;
        let mut discovery = PluginDiscovery::default();
        let mut seen_ids = BTreeSet::<String>::new();
        let mut seen_paths = BTreeSet::<PathBuf>::new();
        let mut stale_registry_ids = Vec::new();

        for install_path in discover_plugin_dirs(&self.install_root())? {
            let matched_record = registry
                .plugins
                .values()
                .find(|record| record.install_path == install_path);
            let kind = matched_record.map_or(PluginKind::External, |record| record.kind);
            let source = matched_record.map_or_else(
                || install_path.display().to_string(),
                |record| describe_install_source(&record.source),
            );
            match load_plugin_definition(&install_path, kind, source.clone(), kind.marketplace()) {
                Ok(plugin) => {
                    if seen_ids.insert(plugin.metadata().id.clone()) {
                        seen_paths.insert(install_path);
                        discovery.push_plugin(plugin);
                    }
                }
                Err(error) => {
                    discovery.push_failure(PluginLoadFailure::new(
                        install_path,
                        kind,
                        source,
                        error,
                    ));
                }
            }
        }

        for record in registry.plugins.values() {
            if seen_paths.contains(&record.install_path) {
                continue;
            }
            if !record.install_path.exists() || plugin_manifest_path(&record.install_path).is_err()
            {
                stale_registry_ids.push(record.id.clone());
                continue;
            }
            let source = describe_install_source(&record.source);
            match load_plugin_definition(
                &record.install_path,
                record.kind,
                source.clone(),
                record.kind.marketplace(),
            ) {
                Ok(plugin) => {
                    if seen_ids.insert(plugin.metadata().id.clone()) {
                        seen_paths.insert(record.install_path.clone());
                        discovery.push_plugin(plugin);
                    }
                }
                Err(error) => {
                    discovery.push_failure(PluginLoadFailure::new(
                        record.install_path.clone(),
                        record.kind,
                        source,
                        error,
                    ));
                }
            }
        }

        if !stale_registry_ids.is_empty() {
            for plugin_id in stale_registry_ids {
                registry.plugins.remove(&plugin_id);
            }
            self.store_registry(&registry)?;
        }

        Ok(discovery)
    }

    fn discover_external_directory_plugins_with_failures(
        &self,
        existing_plugins: &[PluginDefinition],
    ) -> Result<PluginDiscovery, PluginError> {
        let mut discovery = PluginDiscovery::default();

        for directory in &self.config.external_dirs {
            for root in discover_plugin_dirs(directory)? {
                let source = root.display().to_string();
                match load_plugin_definition(
                    &root,
                    PluginKind::External,
                    source.clone(),
                    EXTERNAL_MARKETPLACE,
                ) {
                    Ok(plugin) => {
                        if existing_plugins
                            .iter()
                            .chain(discovery.plugins.iter())
                            .all(|existing| existing.metadata().id != plugin.metadata().id)
                        {
                            discovery.push_plugin(plugin);
                        }
                    }
                    Err(error) => {
                        discovery.push_failure(PluginLoadFailure::new(
                            root,
                            PluginKind::External,
                            source,
                            error,
                        ));
                    }
                }
            }
        }

        Ok(discovery)
    }

    pub fn installed_plugin_registry_report(&self) -> Result<PluginRegistryReport, PluginError> {
        self.sync_bundled_plugins()?;
        Ok(self.build_registry_report(self.discover_installed_plugins_with_failures()?))
    }

    fn sync_bundled_plugins(&self) -> Result<(), PluginError> {
        let explicit_root = self.config.bundled_root.is_some();
        let bundled_root = self
            .config
            .bundled_root
            .clone()
            .unwrap_or_else(Self::bundled_root);
        let bundled_plugins = match discover_plugin_dirs(&bundled_root) {
            Ok(plugins) => plugins,
            // When the bundled root is the auto-detected default and the directory is
            // inaccessible (e.g. a root-owned source tree), treat it as empty rather
            // than fatally failing.  An explicit config override still surfaces errors.
            Err(PluginError::Io(ref error))
                if !explicit_root && error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        let mut registry = self.load_registry()?;
        let mut changed = false;
        let install_root = self.install_root();
        let mut active_bundled_ids = BTreeSet::new();

        for source_root in bundled_plugins {
            let manifest = load_plugin_from_directory(&source_root)?;
            let plugin_id = plugin_id(&manifest.name, BUNDLED_MARKETPLACE);
            active_bundled_ids.insert(plugin_id.clone());
            let install_path = install_root.join(sanitize_plugin_id(&plugin_id));
            let now = unix_time_ms();
            let existing_record = registry.plugins.get(&plugin_id);
            let installed_copy_is_valid =
                install_path.exists() && load_plugin_from_directory(&install_path).is_ok();
            let needs_sync = existing_record.is_none_or(|record| {
                record.kind != PluginKind::Bundled
                    || record.version != manifest.version
                    || record.name != manifest.name
                    || record.description != manifest.description
                    || record.install_path != install_path
                    || !record.install_path.exists()
                    || !installed_copy_is_valid
            });

            if !needs_sync {
                continue;
            }

            if install_path.exists() {
                fs::remove_dir_all(&install_path)?;
            }
            copy_dir_all(&source_root, &install_path)?;

            let installed_at_unix_ms =
                existing_record.map_or(now, |record| record.installed_at_unix_ms);
            registry.plugins.insert(
                plugin_id.clone(),
                InstalledPluginRecord {
                    kind: PluginKind::Bundled,
                    id: plugin_id,
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    install_path,
                    source: PluginInstallSource::LocalPath { path: source_root },
                    installed_at_unix_ms,
                    updated_at_unix_ms: now,
                },
            );
            changed = true;
        }

        let stale_bundled_ids = registry
            .plugins
            .iter()
            .filter_map(|(plugin_id, record)| {
                (record.kind == PluginKind::Bundled && !active_bundled_ids.contains(plugin_id))
                    .then_some(plugin_id.clone())
            })
            .collect::<Vec<_>>();

        for plugin_id in stale_bundled_ids {
            if let Some(record) = registry.plugins.remove(&plugin_id) {
                if record.install_path.exists() {
                    fs::remove_dir_all(&record.install_path)?;
                }
                changed = true;
            }
        }

        if changed {
            self.store_registry(&registry)?;
        }

        Ok(())
    }

    fn is_enabled(&self, metadata: &PluginMetadata) -> bool {
        self.config
            .enabled_plugins
            .get(&metadata.id)
            .copied()
            .unwrap_or(match metadata.kind {
                PluginKind::External => false,
                PluginKind::Builtin | PluginKind::Bundled => metadata.default_enabled,
            })
    }

    fn ensure_known_plugin(&self, plugin_id: &str) -> Result<(), PluginError> {
        if self.plugin_registry()?.contains(plugin_id) {
            Ok(())
        } else {
            Err(PluginError::NotFound(format!(
                "plugin `{plugin_id}` is not installed or discoverable"
            )))
        }
    }

    fn load_registry(&self) -> Result<InstalledPluginRegistry, PluginError> {
        let path = self.registry_path();
        match fs::read_to_string(&path) {
            Ok(contents) if contents.trim().is_empty() => Ok(InstalledPluginRegistry::default()),
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(InstalledPluginRegistry::default())
            }
            Err(error) => Err(PluginError::Io(error)),
        }
    }

    fn store_registry(&self, registry: &InstalledPluginRegistry) -> Result<(), PluginError> {
        let path = self.registry_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(registry)?)?;
        Ok(())
    }

    fn write_enabled_state(
        &self,
        plugin_id: &str,
        enabled: Option<bool>,
    ) -> Result<(), PluginError> {
        update_settings_json(&self.settings_path(), |root| {
            let enabled_plugins = ensure_object(root, "enabledPlugins");
            match enabled {
                Some(value) => {
                    enabled_plugins.insert(plugin_id.to_string(), Value::Bool(value));
                }
                None => {
                    enabled_plugins.remove(plugin_id);
                }
            }
        })
    }

    fn installed_plugin_registry(&self) -> Result<PluginRegistry, PluginError> {
        self.installed_plugin_registry_report()?.into_registry()
    }

    fn build_registry_report(&self, discovery: PluginDiscovery) -> PluginRegistryReport {
        PluginRegistryReport::new(
            PluginRegistry::new(
                discovery
                    .plugins
                    .into_iter()
                    .map(|plugin| {
                        let enabled = self.is_enabled(plugin.metadata());
                        RegisteredPlugin::new(plugin, enabled)
                    })
                    .collect(),
            ),
            discovery.failures,
        )
    }
}

#[must_use]
pub fn builtin_plugins() -> Vec<PluginDefinition> {
    vec![PluginDefinition::Builtin(BuiltinPlugin {
        metadata: PluginMetadata {
            id: plugin_id("example-builtin", BUILTIN_MARKETPLACE),
            name: "example-builtin".to_string(),
            version: "0.1.0".to_string(),
            description: "Example built-in plugin scaffold for the Rust plugin system".to_string(),
            kind: PluginKind::Builtin,
            source: BUILTIN_MARKETPLACE.to_string(),
            default_enabled: false,
            root: None,
        },
        hooks: PluginHooks::default(),
        lifecycle: PluginLifecycle::default(),
        tools: Vec::new(),
    })]
}

fn load_plugin_definition(
    root: &Path,
    kind: PluginKind,
    source: String,
    marketplace: &str,
) -> Result<PluginDefinition, PluginError> {
    let manifest = load_plugin_from_directory(root)?;
    let metadata = PluginMetadata {
        id: plugin_id(&manifest.name, marketplace),
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        kind,
        source,
        default_enabled: manifest.default_enabled,
        root: Some(root.to_path_buf()),
    };
    let hooks = resolve_hooks(root, &manifest.hooks);
    let lifecycle = resolve_lifecycle(root, &manifest.lifecycle);
    let tools = resolve_tools(root, &metadata.id, &metadata.name, &manifest.tools);
    Ok(match kind {
        PluginKind::Builtin => PluginDefinition::Builtin(BuiltinPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
        PluginKind::Bundled => PluginDefinition::Bundled(BundledPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
        PluginKind::External => PluginDefinition::External(ExternalPlugin {
            metadata,
            hooks,
            lifecycle,
            tools,
        }),
    })
}

pub fn load_plugin_from_directory(root: &Path) -> Result<PluginManifest, PluginError> {
    load_manifest_from_directory(root)
}

fn load_manifest_from_directory(root: &Path) -> Result<PluginManifest, PluginError> {
    let manifest_path = plugin_manifest_path(root)?;
    load_manifest_from_path(root, &manifest_path)
}

fn load_manifest_from_path(
    root: &Path,
    manifest_path: &Path,
) -> Result<PluginManifest, PluginError> {
    let contents = fs::read_to_string(manifest_path).map_err(|error| {
        PluginError::NotFound(format!(
            "plugin manifest not found at {}: {error}",
            manifest_path.display()
        ))
    })?;
    let raw_json: Value = serde_json::from_str(&contents)?;
    let compatibility_errors = detect_claude_code_manifest_contract_gaps(&raw_json);
    if !compatibility_errors.is_empty() {
        return Err(PluginError::ManifestValidation(compatibility_errors));
    }
    let raw_manifest: RawPluginManifest = serde_json::from_value(raw_json)?;
    build_plugin_manifest(root, raw_manifest)
}

fn detect_claude_code_manifest_contract_gaps(
    raw_manifest: &Value,
) -> Vec<PluginManifestValidationError> {
    let Some(root) = raw_manifest.as_object() else {
        return Vec::new();
    };

    let mut errors = Vec::new();

    for (field, detail) in [
        (
            "skills",
            "plugin manifest field `skills` uses the Claude Code plugin contract; `claw` does not load plugin-managed skills and instead discovers skills from local roots such as `.claw/skills`, `.omc/skills`, `.agents/skills`, `~/.omc/skills`, and `~/.claude/skills/omc-learned`.",
        ),
        (
            "mcpServers",
            "plugin manifest field `mcpServers` uses the Claude Code plugin contract; `claw` does not import MCP servers from plugin manifests.",
        ),
        (
            "agents",
            "plugin manifest field `agents` uses the Claude Code plugin contract; `claw` does not load plugin-managed agent markdown catalogs from plugin manifests.",
        ),
    ] {
        if root.contains_key(field) {
            errors.push(PluginManifestValidationError::UnsupportedManifestContract {
                detail: detail.to_string(),
            });
        }
    }

    if root
        .get("commands")
        .and_then(Value::as_array)
        .is_some_and(|commands| commands.iter().any(Value::is_string))
    {
        errors.push(PluginManifestValidationError::UnsupportedManifestContract {
            detail: "plugin manifest field `commands` uses Claude Code-style directory globs; `claw` slash dispatch is still built-in and does not load plugin slash command markdown files.".to_string(),
        });
    }

    if let Some(hooks) = root.get("hooks").and_then(Value::as_object) {
        for hook_name in hooks.keys() {
            if !matches!(
                hook_name.as_str(),
                "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
            ) {
                errors.push(PluginManifestValidationError::UnsupportedManifestContract {
                    detail: format!(
                        "plugin hook `{hook_name}` uses the Claude Code lifecycle contract; `claw` plugins currently support only PreToolUse, PostToolUse, and PostToolUseFailure."
                    ),
                });
            }
        }
    }

    errors
}

fn plugin_manifest_path(root: &Path) -> Result<PathBuf, PluginError> {
    let direct_path = root.join(MANIFEST_FILE_NAME);
    if direct_path.exists() {
        return Ok(direct_path);
    }

    let packaged_path = root.join(MANIFEST_RELATIVE_PATH);
    if packaged_path.exists() {
        return Ok(packaged_path);
    }

    Err(PluginError::NotFound(format!(
        "plugin manifest not found at {} or {}",
        direct_path.display(),
        packaged_path.display()
    )))
}

fn build_plugin_manifest(
    root: &Path,
    raw: RawPluginManifest,
) -> Result<PluginManifest, PluginError> {
    let mut errors = Vec::new();

    validate_required_manifest_field("name", &raw.name, &mut errors);
    validate_required_manifest_field("version", &raw.version, &mut errors);
    validate_required_manifest_field("description", &raw.description, &mut errors);

    let permissions = build_manifest_permissions(&raw.permissions, &mut errors);
    validate_command_entries(root, raw.hooks.pre_tool_use.iter(), "hook", &mut errors);
    validate_command_entries(root, raw.hooks.post_tool_use.iter(), "hook", &mut errors);
    validate_command_entries(
        root,
        raw.hooks.post_tool_use_failure.iter(),
        "hook",
        &mut errors,
    );
    validate_command_entries(
        root,
        raw.lifecycle.init.iter(),
        "lifecycle command",
        &mut errors,
    );
    validate_command_entries(
        root,
        raw.lifecycle.shutdown.iter(),
        "lifecycle command",
        &mut errors,
    );
    let tools = build_manifest_tools(root, raw.tools, &mut errors);
    let commands = build_manifest_commands(root, raw.commands, &mut errors);

    if !errors.is_empty() {
        return Err(PluginError::ManifestValidation(errors));
    }

    Ok(PluginManifest {
        name: raw.name,
        version: raw.version,
        description: raw.description,
        permissions,
        default_enabled: raw.default_enabled,
        hooks: raw.hooks,
        lifecycle: raw.lifecycle,
        tools,
        commands,
    })
}

fn validate_required_manifest_field(
    field: &'static str,
    value: &str,
    errors: &mut Vec<PluginManifestValidationError>,
) {
    if value.trim().is_empty() {
        errors.push(PluginManifestValidationError::EmptyField { field });
    }
}

fn build_manifest_permissions(
    permissions: &[String],
    errors: &mut Vec<PluginManifestValidationError>,
) -> Vec<PluginPermission> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::new();

    for permission in permissions {
        let permission = permission.trim();
        if permission.is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "permission",
                field: "value",
                name: None,
            });
            continue;
        }
        if !seen.insert(permission.to_string()) {
            errors.push(PluginManifestValidationError::DuplicatePermission {
                permission: permission.to_string(),
            });
            continue;
        }
        match PluginPermission::parse(permission) {
            Some(permission) => validated.push(permission),
            None => errors.push(PluginManifestValidationError::InvalidPermission {
                permission: permission.to_string(),
            }),
        }
    }

    validated
}

fn build_manifest_tools(
    root: &Path,
    tools: Vec<RawPluginToolManifest>,
    errors: &mut Vec<PluginManifestValidationError>,
) -> Vec<PluginToolManifest> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::new();

    for tool in tools {
        let name = tool.name.trim().to_string();
        if name.is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "tool",
                field: "name",
                name: None,
            });
            continue;
        }
        if !seen.insert(name.clone()) {
            errors.push(PluginManifestValidationError::DuplicateEntry { kind: "tool", name });
            continue;
        }
        if tool.description.trim().is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "tool",
                field: "description",
                name: Some(name.clone()),
            });
        }
        if tool.command.trim().is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "tool",
                field: "command",
                name: Some(name.clone()),
            });
        } else {
            validate_command_entry(root, &tool.command, "tool", errors);
        }
        if !tool.input_schema.is_object() {
            errors.push(PluginManifestValidationError::InvalidToolInputSchema {
                tool_name: name.clone(),
            });
        }
        let Some(required_permission) =
            PluginToolPermission::parse(tool.required_permission.trim())
        else {
            errors.push(
                PluginManifestValidationError::InvalidToolRequiredPermission {
                    tool_name: name.clone(),
                    permission: tool.required_permission.trim().to_string(),
                },
            );
            continue;
        };

        validated.push(PluginToolManifest {
            name,
            description: tool.description,
            input_schema: tool.input_schema,
            command: tool.command,
            args: tool.args,
            required_permission,
        });
    }

    validated
}

fn build_manifest_commands(
    root: &Path,
    commands: Vec<PluginCommandManifest>,
    errors: &mut Vec<PluginManifestValidationError>,
) -> Vec<PluginCommandManifest> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::new();

    for command in commands {
        let name = command.name.trim().to_string();
        if name.is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "command",
                field: "name",
                name: None,
            });
            continue;
        }
        if !seen.insert(name.clone()) {
            errors.push(PluginManifestValidationError::DuplicateEntry {
                kind: "command",
                name,
            });
            continue;
        }
        if command.description.trim().is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "command",
                field: "description",
                name: Some(name.clone()),
            });
        }
        if command.command.trim().is_empty() {
            errors.push(PluginManifestValidationError::EmptyEntryField {
                kind: "command",
                field: "command",
                name: Some(name.clone()),
            });
        } else {
            validate_command_entry(root, &command.command, "command", errors);
        }
        validated.push(command);
    }

    validated
}

fn validate_command_entries<'a>(
    root: &Path,
    entries: impl Iterator<Item = &'a String>,
    kind: &'static str,
    errors: &mut Vec<PluginManifestValidationError>,
) {
    for entry in entries {
        validate_command_entry(root, entry, kind, errors);
    }
}

fn validate_command_entry(
    root: &Path,
    entry: &str,
    kind: &'static str,
    errors: &mut Vec<PluginManifestValidationError>,
) {
    if entry.trim().is_empty() {
        errors.push(PluginManifestValidationError::EmptyEntryField {
            kind,
            field: "command",
            name: None,
        });
        return;
    }
    if is_literal_command(entry) {
        return;
    }

    let path = if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else {
        root.join(entry)
    };
    if !path.exists() {
        errors.push(PluginManifestValidationError::MissingPath { kind, path });
    } else if !path.is_file() {
        errors.push(PluginManifestValidationError::PathIsDirectory { kind, path });
    }
}

fn resolve_hooks(root: &Path, hooks: &PluginHooks) -> PluginHooks {
    PluginHooks {
        pre_tool_use: hooks
            .pre_tool_use
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
        post_tool_use: hooks
            .post_tool_use
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
        post_tool_use_failure: hooks
            .post_tool_use_failure
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
    }
}

fn resolve_lifecycle(root: &Path, lifecycle: &PluginLifecycle) -> PluginLifecycle {
    PluginLifecycle {
        init: lifecycle
            .init
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
        shutdown: lifecycle
            .shutdown
            .iter()
            .map(|entry| resolve_hook_entry(root, entry))
            .collect(),
    }
}

fn resolve_tools(
    root: &Path,
    plugin_id: &str,
    plugin_name: &str,
    tools: &[PluginToolManifest],
) -> Vec<PluginTool> {
    tools
        .iter()
        .map(|tool| {
            PluginTool::new(
                plugin_id,
                plugin_name,
                PluginToolDefinition {
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    input_schema: tool.input_schema.clone(),
                },
                resolve_hook_entry(root, &tool.command),
                tool.args.clone(),
                tool.required_permission,
                Some(root.to_path_buf()),
            )
        })
        .collect()
}

fn validate_hook_paths(root: Option<&Path>, hooks: &PluginHooks) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for entry in hooks
        .pre_tool_use
        .iter()
        .chain(hooks.post_tool_use.iter())
        .chain(hooks.post_tool_use_failure.iter())
    {
        validate_command_path(root, entry, "hook")?;
    }
    Ok(())
}

fn validate_lifecycle_paths(
    root: Option<&Path>,
    lifecycle: &PluginLifecycle,
) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for entry in lifecycle.init.iter().chain(lifecycle.shutdown.iter()) {
        validate_command_path(root, entry, "lifecycle command")?;
    }
    Ok(())
}

fn validate_tool_paths(root: Option<&Path>, tools: &[PluginTool]) -> Result<(), PluginError> {
    let Some(root) = root else {
        return Ok(());
    };
    for tool in tools {
        validate_command_path(root, &tool.command, "tool")?;
    }
    Ok(())
}

fn validate_command_path(root: &Path, entry: &str, kind: &str) -> Result<(), PluginError> {
    if is_literal_command(entry) {
        return Ok(());
    }
    let path = if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else {
        root.join(entry)
    };
    if !path.exists() {
        return Err(PluginError::InvalidManifest(format!(
            "{kind} path `{}` does not exist",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "{kind} path `{}` must point to a file",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_hook_entry(root: &Path, entry: &str) -> String {
    if is_literal_command(entry) {
        entry.to_string()
    } else {
        root.join(entry).display().to_string()
    }
}

fn is_literal_command(entry: &str) -> bool {
    !entry.starts_with("./") && !entry.starts_with("../") && !Path::new(entry).is_absolute()
}

fn run_lifecycle_commands(
    metadata: &PluginMetadata,
    lifecycle: &PluginLifecycle,
    phase: &str,
    commands: &[String],
) -> Result<(), PluginError> {
    if lifecycle.is_empty() || commands.is_empty() {
        return Ok(());
    }

    for command in commands {
        let mut process = if Path::new(command).exists() {
            if cfg!(windows) {
                let mut process = Command::new("cmd");
                process.arg("/C").arg(command);
                process
            } else {
                let mut process = Command::new("sh");
                process.arg(command);
                process
            }
        } else if cfg!(windows) {
            let mut process = Command::new("cmd");
            process.arg("/C").arg(command);
            process
        } else {
            let mut process = Command::new("sh");
            process.arg("-lc").arg(command);
            process
        };
        if let Some(root) = &metadata.root {
            process.current_dir(root);
        }
        let output = process.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(PluginError::CommandFailed(format!(
                "plugin `{}` {} failed for `{}`: {}",
                metadata.id,
                phase,
                command,
                if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                }
            )));
        }
    }

    Ok(())
}

fn resolve_local_source(source: &str) -> Result<PathBuf, PluginError> {
    let path = PathBuf::from(source);
    if path.exists() {
        Ok(path)
    } else {
        Err(PluginError::NotFound(format!(
            "plugin source `{source}` was not found"
        )))
    }
}

fn parse_install_source(source: &str) -> Result<PluginInstallSource, PluginError> {
    if source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || Path::new(source)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
    {
        Ok(PluginInstallSource::GitUrl {
            url: source.to_string(),
        })
    } else {
        Ok(PluginInstallSource::LocalPath {
            path: resolve_local_source(source)?,
        })
    }
}

fn materialize_source(
    source: &PluginInstallSource,
    temp_root: &Path,
) -> Result<PathBuf, PluginError> {
    fs::create_dir_all(temp_root)?;
    match source {
        PluginInstallSource::LocalPath { path } => Ok(path.clone()),
        PluginInstallSource::GitUrl { url } => {
            static MATERIALIZE_COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = MATERIALIZE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let destination = temp_root.join(format!("plugin-{nanos}-{unique}"));
            let output = Command::new("git")
                .arg("clone")
                .arg("--depth")
                .arg("1")
                .arg(url)
                .arg(&destination)
                .output()?;
            if !output.status.success() {
                return Err(PluginError::CommandFailed(format!(
                    "git clone failed for `{url}`: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(destination)
        }
    }
}

fn discover_plugin_dirs(root: &Path) -> Result<Vec<PathBuf>, PluginError> {
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut paths = Vec::new();
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() && plugin_manifest_path(&path).is_ok() {
                    paths.push(path);
                }
            }
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(PluginError::Io(error)),
    }
}

fn plugin_id(name: &str, marketplace: &str) -> String {
    format!("{name}@{marketplace}")
}

fn sanitize_plugin_id(plugin_id: &str) -> String {
    plugin_id
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '@' | ':' => '-',
            other => other,
        })
        .collect()
}

fn describe_install_source(source: &PluginInstallSource) -> String {
    match source {
        PluginInstallSource::LocalPath { path } => path.display().to_string(),
        PluginInstallSource::GitUrl { url } => url.clone(),
    }
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_millis()
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<(), PluginError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn update_settings_json(
    path: &Path,
    mut update: impl FnMut(&mut Map<String, Value>),
) -> Result<(), PluginError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = match fs::read_to_string(path) {
        Ok(contents) if !contents.trim().is_empty() => serde_json::from_str::<Value>(&contents)?,
        Ok(_) => Value::Object(Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => return Err(PluginError::Io(error)),
    };

    let object = root.as_object_mut().ok_or_else(|| {
        PluginError::InvalidManifest(format!(
            "settings file {} must contain a JSON object",
            path.display()
        ))
    })?;
    update(object);
    fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn ensure_object<'a>(root: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !root.get(key).is_some_and(Value::is_object) {
        root.insert(key.to_string(), Value::Object(Map::new()));
    }
    root.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object should exist")
}

/// Environment variable lock for test isolation.
/// Guards against concurrent modification of `CLAW_CONFIG_HOME`.
#[cfg(test)]
fn env_lock() -> &'static std::sync::Mutex<()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &ENV_LOCK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("plugins-{label}-{nanos}"))
    }

    #[test]
    fn env_guard_recovers_after_poisoning() {
        let poisoned = std::thread::spawn(|| {
            let _guard = env_guard();
            panic!("poison env lock");
        })
        .join();
        assert!(poisoned.is_err(), "poisoning thread should panic");

        let _guard = env_guard();
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    fn write_loader_plugin(root: &Path) {
        write_file(
            root.join("hooks").join("pre.sh").as_path(),
            "#!/bin/sh\nprintf 'pre'\n",
        );
        write_file(
            root.join("tools").join("echo-tool.sh").as_path(),
            "#!/bin/sh\ncat\n",
        );
        write_file(
            root.join("commands").join("sync.sh").as_path(),
            "#!/bin/sh\nprintf 'sync'\n",
        );
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "loader-demo",
  "version": "1.2.3",
  "description": "Manifest loader test plugin",
  "permissions": ["read", "write"],
  "hooks": {
    "PreToolUse": ["./hooks/pre.sh"]
  },
  "tools": [
    {
      "name": "echo_tool",
      "description": "Echoes JSON input",
      "inputSchema": {
        "type": "object"
      },
      "command": "./tools/echo-tool.sh",
      "requiredPermission": "workspace-write"
    }
  ],
  "commands": [
    {
      "name": "sync",
      "description": "Sync command",
      "command": "./commands/sync.sh"
    }
  ]
}"#,
        );
    }

    fn write_external_plugin(root: &Path, name: &str, version: &str) {
        write_file(
            root.join("hooks").join("pre.sh").as_path(),
            "#!/bin/sh\nprintf 'pre'\n",
        );
        write_file(
            root.join("hooks").join("post.sh").as_path(),
            "#!/bin/sh\nprintf 'post'\n",
        );
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"test plugin\",\n  \"hooks\": {{\n    \"PreToolUse\": [\"./hooks/pre.sh\"],\n    \"PostToolUse\": [\"./hooks/post.sh\"]\n  }}\n}}"
            )
            .as_str(),
        );
    }

    fn write_broken_plugin(root: &Path, name: &str) {
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\",\n  \"description\": \"broken plugin\",\n  \"hooks\": {{\n    \"PreToolUse\": [\"./hooks/missing.sh\"]\n  }}\n}}"
            )
            .as_str(),
        );
    }

    fn write_directory_path_plugin(root: &Path, name: &str) {
        fs::create_dir_all(root.join("hooks").join("pre-dir")).expect("hook dir");
        fs::create_dir_all(root.join("tools").join("tool-dir")).expect("tool dir");
        fs::create_dir_all(root.join("commands").join("sync-dir")).expect("command dir");
        fs::create_dir_all(root.join("lifecycle").join("init-dir")).expect("lifecycle dir");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\",\n  \"description\": \"directory path plugin\",\n  \"hooks\": {{\n    \"PreToolUse\": [\"./hooks/pre-dir\"]\n  }},\n  \"lifecycle\": {{\n    \"Init\": [\"./lifecycle/init-dir\"]\n  }},\n  \"tools\": [\n    {{\n      \"name\": \"dir_tool\",\n      \"description\": \"Directory tool\",\n      \"inputSchema\": {{\"type\": \"object\"}},\n      \"command\": \"./tools/tool-dir\"\n    }}\n  ],\n  \"commands\": [\n    {{\n      \"name\": \"sync\",\n      \"description\": \"Directory command\",\n      \"command\": \"./commands/sync-dir\"\n    }}\n  ]\n}}"
            )
            .as_str(),
        );
    }

    fn write_broken_failure_hook_plugin(root: &Path, name: &str) {
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\",\n  \"description\": \"broken plugin\",\n  \"hooks\": {{\n    \"PostToolUseFailure\": [\"./hooks/missing-failure.sh\"]\n  }}\n}}"
            )
            .as_str(),
        );
    }

    fn write_lifecycle_plugin(root: &Path, name: &str, version: &str) -> PathBuf {
        let log_path = root.join("lifecycle.log");
        write_file(
            root.join("lifecycle").join("init.sh").as_path(),
            "#!/bin/sh\nprintf 'init\\n' >> lifecycle.log\n",
        );
        write_file(
            root.join("lifecycle").join("shutdown.sh").as_path(),
            "#!/bin/sh\nprintf 'shutdown\\n' >> lifecycle.log\n",
        );
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"lifecycle plugin\",\n  \"lifecycle\": {{\n    \"Init\": [\"./lifecycle/init.sh\"],\n    \"Shutdown\": [\"./lifecycle/shutdown.sh\"]\n  }}\n}}"
            )
            .as_str(),
        );
        log_path
    }

    fn write_tool_plugin(root: &Path, name: &str, version: &str) {
        write_tool_plugin_with_name(root, name, version, "plugin_echo");
    }

    fn write_tool_plugin_with_name(root: &Path, name: &str, version: &str, tool_name: &str) {
        let script_path = root.join("tools").join("echo-json.sh");
        write_file(
            &script_path,
            "#!/bin/sh\nINPUT=$(cat)\nprintf '{\"plugin\":\"%s\",\"tool\":\"%s\",\"input\":%s}\\n' \"$CLAWD_PLUGIN_ID\" \"$CLAWD_TOOL_NAME\" \"$INPUT\"\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("chmod");
        }
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"tool plugin\",\n  \"tools\": [\n    {{\n      \"name\": \"{tool_name}\",\n      \"description\": \"Echo JSON input\",\n      \"inputSchema\": {{\"type\": \"object\", \"properties\": {{\"message\": {{\"type\": \"string\"}}}}, \"required\": [\"message\"], \"additionalProperties\": false}},\n      \"command\": \"./tools/echo-json.sh\",\n      \"requiredPermission\": \"workspace-write\"\n    }}\n  ]\n}}"
            )
            .as_str(),
        );
    }

    fn write_bundled_plugin(root: &Path, name: &str, version: &str, default_enabled: bool) {
        write_file(
            root.join(MANIFEST_RELATIVE_PATH).as_path(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"bundled plugin\",\n  \"defaultEnabled\": {}\n}}",
                if default_enabled { "true" } else { "false" }
            )
            .as_str(),
        );
    }

    fn load_enabled_plugins(path: &Path) -> BTreeMap<String, bool> {
        let contents = fs::read_to_string(path).expect("settings should exist");
        let root: Value = serde_json::from_str(&contents).expect("settings json");
        root.get("enabledPlugins")
            .and_then(Value::as_object)
            .map(|enabled_plugins| {
                enabled_plugins
                    .iter()
                    .map(|(plugin_id, value)| {
                        (
                            plugin_id.clone(),
                            value.as_bool().expect("plugin state should be a bool"),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn load_plugin_from_directory_validates_required_fields() {
        let _guard = env_guard();
        let root = temp_dir("manifest-required");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{"name":"","version":"1.0.0","description":"desc"}"#,
        );

        let error = load_plugin_from_directory(&root).expect_err("empty name should fail");
        assert!(error.to_string().contains("name cannot be empty"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_reads_root_manifest_and_validates_entries() {
        let _guard = env_guard();
        let root = temp_dir("manifest-root");
        write_loader_plugin(&root);

        let manifest = load_plugin_from_directory(&root).expect("manifest should load");
        assert_eq!(manifest.name, "loader-demo");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(
            manifest
                .permissions
                .iter()
                .map(|permission| permission.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "write"]
        );
        assert_eq!(manifest.hooks.pre_tool_use, vec!["./hooks/pre.sh"]);
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "echo_tool");
        assert_eq!(
            manifest.tools[0].required_permission,
            PluginToolPermission::WorkspaceWrite
        );
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(manifest.commands[0].name, "sync");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_supports_packaged_manifest_path() {
        let _guard = env_guard();
        let root = temp_dir("manifest-packaged");
        write_external_plugin(&root, "packaged-demo", "1.0.0");

        let manifest = load_plugin_from_directory(&root).expect("packaged manifest should load");
        assert_eq!(manifest.name, "packaged-demo");
        assert!(manifest.tools.is_empty());
        assert!(manifest.commands.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_defaults_optional_fields() {
        let _guard = env_guard();
        let root = temp_dir("manifest-defaults");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "minimal",
  "version": "0.1.0",
  "description": "Minimal manifest"
}"#,
        );

        let manifest = load_plugin_from_directory(&root).expect("minimal manifest should load");
        assert!(manifest.permissions.is_empty());
        assert!(manifest.hooks.is_empty());
        assert!(manifest.tools.is_empty());
        assert!(manifest.commands.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_duplicate_permissions_and_commands() {
        let _guard = env_guard();
        let root = temp_dir("manifest-duplicates");
        write_file(
            root.join("commands").join("sync.sh").as_path(),
            "#!/bin/sh\nprintf 'sync'\n",
        );
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "duplicate-manifest",
  "version": "1.0.0",
  "description": "Duplicate validation",
  "permissions": ["read", "read"],
  "commands": [
    {"name": "sync", "description": "Sync one", "command": "./commands/sync.sh"},
    {"name": "sync", "description": "Sync two", "command": "./commands/sync.sh"}
  ]
}"#,
        );

        let error = load_plugin_from_directory(&root).expect_err("duplicates should fail");
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::DuplicatePermission { permission }
                    if permission == "read"
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::DuplicateEntry { kind, name }
                    if *kind == "command" && name == "sync"
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_claude_code_manifest_contracts_with_guidance() {
        let root = temp_dir("manifest-claude-code-contract");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "oh-my-claudecode",
  "version": "4.10.2",
  "description": "Claude Code plugin manifest",
  "hooks": {
    "SessionStart": ["scripts/session-start.mjs"]
  },
  "agents": ["agents/*.md"],
  "commands": ["commands/**/*.md"],
  "skills": "./skills/",
  "mcpServers": "./.mcp.json"
}"#,
        );

        let error = load_plugin_from_directory(&root)
            .expect_err("Claude Code plugin manifest should fail with guidance");
        let rendered = error.to_string();
        assert!(rendered.contains("field `skills` uses the Claude Code plugin contract"));
        assert!(rendered.contains("field `mcpServers` uses the Claude Code plugin contract"));
        assert!(rendered.contains("field `agents` uses the Claude Code plugin contract"));
        assert!(rendered.contains("field `commands` uses Claude Code-style directory globs"));
        assert!(rendered.contains("hook `SessionStart` uses the Claude Code lifecycle contract"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_missing_tool_or_command_paths() {
        let root = temp_dir("manifest-paths");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "missing-paths",
  "version": "1.0.0",
  "description": "Missing path validation",
  "tools": [
    {
      "name": "tool_one",
      "description": "Missing tool script",
      "inputSchema": {"type": "object"},
      "command": "./tools/missing.sh"
    }
  ]
}"#,
        );

        let error = load_plugin_from_directory(&root).expect_err("missing paths should fail");
        assert!(error.to_string().contains("does not exist"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_missing_lifecycle_paths() {
        // given
        let root = temp_dir("manifest-lifecycle-paths");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "missing-lifecycle-paths",
  "version": "1.0.0",
  "description": "Missing lifecycle path validation",
  "lifecycle": {
    "Init": ["./lifecycle/init.sh"],
    "Shutdown": ["./lifecycle/shutdown.sh"]
  }
}"#,
        );

        // when
        let error =
            load_plugin_from_directory(&root).expect_err("missing lifecycle paths should fail");

        // then
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::MissingPath { kind, path }
                    if *kind == "lifecycle command"
                        && path.ends_with(Path::new("lifecycle/init.sh"))
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::MissingPath { kind, path }
                    if *kind == "lifecycle command"
                        && path.ends_with(Path::new("lifecycle/shutdown.sh"))
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_directory_command_paths() {
        // given
        let root = temp_dir("manifest-directory-paths");
        write_directory_path_plugin(&root, "directory-paths");

        // when
        let error =
            load_plugin_from_directory(&root).expect_err("directory command paths should fail");

        // then
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::PathIsDirectory { kind, path }
                    if *kind == "hook" && path.ends_with(Path::new("hooks/pre-dir"))
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::PathIsDirectory { kind, path }
                    if *kind == "lifecycle command"
                        && path.ends_with(Path::new("lifecycle/init-dir"))
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::PathIsDirectory { kind, path }
                    if *kind == "tool" && path.ends_with(Path::new("tools/tool-dir"))
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::PathIsDirectory { kind, path }
                    if *kind == "command" && path.ends_with(Path::new("commands/sync-dir"))
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_invalid_permissions() {
        let root = temp_dir("manifest-invalid-permissions");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "invalid-permissions",
  "version": "1.0.0",
  "description": "Invalid permission validation",
  "permissions": ["admin"]
}"#,
        );

        let error = load_plugin_from_directory(&root).expect_err("invalid permissions should fail");
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::InvalidPermission { permission }
                    if permission == "admin"
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_rejects_invalid_tool_required_permission() {
        let root = temp_dir("manifest-invalid-tool-permission");
        write_file(
            root.join("tools").join("echo.sh").as_path(),
            "#!/bin/sh\ncat\n",
        );
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "invalid-tool-permission",
  "version": "1.0.0",
  "description": "Invalid tool permission validation",
  "tools": [
    {
      "name": "echo_tool",
      "description": "Echo tool",
      "inputSchema": {"type": "object"},
      "command": "./tools/echo.sh",
      "requiredPermission": "admin"
    }
  ]
}"#,
        );

        let error =
            load_plugin_from_directory(&root).expect_err("invalid tool permission should fail");
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::InvalidToolRequiredPermission {
                        tool_name,
                        permission
                    } if tool_name == "echo_tool" && permission == "admin"
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_plugin_from_directory_accumulates_multiple_validation_errors() {
        let root = temp_dir("manifest-multi-error");
        write_file(
            root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "",
  "version": "1.0.0",
  "description": "",
  "permissions": ["admin"],
  "commands": [
    {"name": "", "description": "", "command": "./commands/missing.sh"}
  ]
}"#,
        );

        let error =
            load_plugin_from_directory(&root).expect_err("multiple manifest errors should fail");
        match error {
            PluginError::ManifestValidation(errors) => {
                assert!(errors.len() >= 4);
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::EmptyField { field } if *field == "name"
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::EmptyField { field }
                    if *field == "description"
                )));
                assert!(errors.iter().any(|error| matches!(
                    error,
                    PluginManifestValidationError::InvalidPermission { permission }
                    if permission == "admin"
                )));
            }
            other => panic!("expected manifest validation errors, got {other}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_builtin_and_bundled_plugins() {
        let _guard = env_guard();
        let manager = PluginManager::new(PluginManagerConfig::new(temp_dir("discover")));
        let plugins = manager.list_plugins().expect("plugins should list");
        assert!(plugins
            .iter()
            .any(|plugin| plugin.metadata.kind == PluginKind::Builtin));
        assert!(plugins
            .iter()
            .any(|plugin| plugin.metadata.kind == PluginKind::Bundled));
    }

    #[test]
    fn installs_enables_updates_and_uninstalls_external_plugins() {
        let _guard = env_guard();
        let config_home = temp_dir("home");
        let source_root = temp_dir("source");
        write_external_plugin(&source_root, "demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install = manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect("install should succeed");
        assert_eq!(install.plugin_id, "demo@external");
        assert!(manager
            .list_plugins()
            .expect("list plugins")
            .iter()
            .any(|plugin| plugin.metadata.id == "demo@external" && plugin.enabled));

        let hooks = manager.aggregated_hooks().expect("hooks should aggregate");
        assert_eq!(hooks.pre_tool_use.len(), 1);
        assert!(hooks.pre_tool_use[0].contains("pre.sh"));

        manager
            .disable("demo@external")
            .expect("disable should work");
        assert!(manager
            .aggregated_hooks()
            .expect("hooks after disable")
            .is_empty());
        manager.enable("demo@external").expect("enable should work");

        write_external_plugin(&source_root, "demo", "2.0.0");
        let update = manager.update("demo@external").expect("update should work");
        assert_eq!(update.old_version, "1.0.0");
        assert_eq!(update.new_version, "2.0.0");

        manager
            .uninstall("demo@external")
            .expect("uninstall should work");
        assert!(!manager
            .list_plugins()
            .expect("list plugins")
            .iter()
            .any(|plugin| plugin.metadata.id == "demo@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn auto_installs_bundled_plugins_into_the_registry() {
        let _guard = env_guard();
        let config_home = temp_dir("bundled-home");
        let bundled_root = temp_dir("bundled-root");
        write_bundled_plugin(&bundled_root.join("starter"), "starter", "0.1.0", false);

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let manager = PluginManager::new(config);

        let installed = manager
            .list_installed_plugins()
            .expect("bundled plugins should auto-install");
        assert!(installed.iter().any(|plugin| {
            plugin.metadata.id == "starter@bundled"
                && plugin.metadata.kind == PluginKind::Bundled
                && !plugin.enabled
        }));

        let registry = manager.load_registry().expect("registry should exist");
        let record = registry
            .plugins
            .get("starter@bundled")
            .expect("bundled plugin should be recorded");
        assert_eq!(record.kind, PluginKind::Bundled);
        assert!(record.install_path.exists());

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn default_bundled_root_loads_repo_bundles_as_installed_plugins() {
        let _guard = env_guard();
        let config_home = temp_dir("default-bundled-home");

        // Use the repo bundled path explicitly so the test is reliable regardless
        // of where the binary runs from.
        let repo_bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled");
        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(repo_bundled.clone());
        let manager = PluginManager::new(config);

        if repo_bundled.exists() {
            let installed = manager
                .list_installed_plugins()
                .expect("bundled plugins should auto-install from repo path");
            assert!(installed
                .iter()
                .any(|plugin| plugin.metadata.id == "example-bundled@bundled"));
            assert!(installed
                .iter()
                .any(|plugin| plugin.metadata.id == "sample-hooks@bundled"));
        }

        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn default_bundled_root_is_not_blindly_cargo_manifest_dir() {
        // Verify that bundled_root() no longer unconditionally returns
        // CARGO_MANIFEST_DIR/bundled.  The returned path must either exist
        // (a valid runtime or dev location was found) OR differ from the
        // compile-time source path (a runtime-relative default was chosen).
        let resolved = PluginManager::bundled_root();
        let compile_time_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled");

        // If the compile-time path does not exist (e.g. installed binary running
        // outside the source tree), the resolved path must NOT be the CARGO_MANIFEST_DIR
        // path, because that would re-introduce the original bug.
        if !compile_time_path.exists() {
            assert_ne!(
                resolved, compile_time_path,
                "bundled_root() must not fall back to CARGO_MANIFEST_DIR when that path \
                 does not exist — this would regress the root-owned-dir permission bug"
            );
        }
        // Either the path exists (dev scenario) or we got a runtime-relative path.
        // Either way the function should not panic or return an obviously wrong value.
        assert!(
            !resolved.as_os_str().is_empty(),
            "bundled_root() should return a non-empty path"
        );
    }

    #[test]
    fn override_bundled_root_is_used_exactly() {
        let _guard = env_guard();
        let config_home = temp_dir("override-bundled-home");
        let bundled_root = temp_dir("override-bundled-root");
        write_bundled_plugin(
            &bundled_root.join("override-plugin"),
            "override-plugin",
            "1.0.0",
            false,
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let manager = PluginManager::new(config);

        let installed = manager
            .list_installed_plugins()
            .expect("override bundled_root should be used");
        assert!(
            installed
                .iter()
                .any(|plugin| plugin.metadata.id == "override-plugin@bundled"),
            "only the override bundled root should be scanned, not CARGO_MANIFEST_DIR"
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn explicit_nonexistent_bundled_root_does_not_fail() {
        // When bundled_root is explicitly configured to a path that does not exist,
        // plugin list should succeed with an empty bundled section rather than
        // returning an error (discover_plugin_dirs treats NotFound as empty).
        let _guard = env_guard();
        let config_home = temp_dir("missing-bundled-home");

        let nonexistent = temp_dir("nonexistent-bundled-XXXXXXXX");
        assert!(
            !nonexistent.exists(),
            "test precondition: path must not exist"
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(nonexistent);
        let manager = PluginManager::new(config);

        // Should succeed with zero bundled plugins, not crash with ENOENT.
        let result = manager.list_installed_plugins();
        assert!(
            result.is_ok(),
            "nonexistent explicit bundled root should not fail: {result:?}"
        );
        let installed = result.unwrap();
        assert!(
            installed
                .iter()
                .all(|p| p.metadata.kind != PluginKind::Bundled),
            "no bundled plugins should be installed when bundled root path does not exist"
        );

        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn no_bundled_root_config_uses_auto_detection_without_panic() {
        // When bundled_root is not set (None), auto-detection runs.  The resolved
        // path should either exist (dev environment) or be a runtime-relative path
        // that doesn't cause a panic or EACCES crash.
        let _guard = env_guard();
        let config_home = temp_dir("auto-detect-bundled-home");

        // No bundled_root set — forces auto-detection in bundled_root().
        let config = PluginManagerConfig::new(&config_home);
        let manager = PluginManager::new(config);

        // Should not panic or return a hard IO error.
        let result = manager.list_installed_plugins();
        assert!(
            result.is_ok(),
            "auto-detected bundled root resolution must not fail: {result:?}"
        );

        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn bundled_sync_prunes_removed_bundled_registry_entries() {
        let _guard = env_guard();
        let config_home = temp_dir("bundled-prune-home");
        let bundled_root = temp_dir("bundled-prune-root");
        let stale_install_path = config_home
            .join("plugins")
            .join("installed")
            .join("stale-bundled-external");
        write_bundled_plugin(&bundled_root.join("active"), "active", "0.1.0", false);
        write_file(
            stale_install_path.join(MANIFEST_RELATIVE_PATH).as_path(),
            r#"{
  "name": "stale",
  "version": "0.1.0",
  "description": "stale bundled plugin"
}"#,
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(config_home.join("plugins").join("installed"));
        let manager = PluginManager::new(config);

        let mut registry = InstalledPluginRegistry::default();
        registry.plugins.insert(
            "stale@bundled".to_string(),
            InstalledPluginRecord {
                kind: PluginKind::Bundled,
                id: "stale@bundled".to_string(),
                name: "stale".to_string(),
                version: "0.1.0".to_string(),
                description: "stale bundled plugin".to_string(),
                install_path: stale_install_path.clone(),
                source: PluginInstallSource::LocalPath {
                    path: bundled_root.join("stale"),
                },
                installed_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            },
        );
        manager.store_registry(&registry).expect("store registry");
        manager
            .write_enabled_state("stale@bundled", Some(true))
            .expect("seed bundled enabled state");

        let installed = manager
            .list_installed_plugins()
            .expect("bundled sync should succeed");
        assert!(installed
            .iter()
            .any(|plugin| plugin.metadata.id == "active@bundled"));
        assert!(!installed
            .iter()
            .any(|plugin| plugin.metadata.id == "stale@bundled"));

        let registry = manager.load_registry().expect("load registry");
        assert!(!registry.plugins.contains_key("stale@bundled"));
        assert!(!stale_install_path.exists());

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn installed_plugin_discovery_keeps_registry_entries_outside_install_root() {
        let _guard = env_guard();
        let config_home = temp_dir("registry-fallback-home");
        let bundled_root = temp_dir("registry-fallback-bundled");
        let install_root = config_home.join("plugins").join("installed");
        let external_install_path = temp_dir("registry-fallback-external");
        write_file(
            external_install_path.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "registry-fallback",
  "version": "1.0.0",
  "description": "Registry fallback plugin"
}"#,
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(install_root.clone());
        let manager = PluginManager::new(config);

        let mut registry = InstalledPluginRegistry::default();
        registry.plugins.insert(
            "registry-fallback@external".to_string(),
            InstalledPluginRecord {
                kind: PluginKind::External,
                id: "registry-fallback@external".to_string(),
                name: "registry-fallback".to_string(),
                version: "1.0.0".to_string(),
                description: "Registry fallback plugin".to_string(),
                install_path: external_install_path.clone(),
                source: PluginInstallSource::LocalPath {
                    path: external_install_path.clone(),
                },
                installed_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            },
        );
        manager.store_registry(&registry).expect("store registry");
        manager
            .write_enabled_state("stale-external@external", Some(true))
            .expect("seed stale external enabled state");

        let installed = manager
            .list_installed_plugins()
            .expect("registry fallback plugin should load");
        assert!(installed
            .iter()
            .any(|plugin| plugin.metadata.id == "registry-fallback@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
        let _ = fs::remove_dir_all(external_install_path);
    }

    #[test]
    fn installed_plugin_discovery_prunes_stale_registry_entries() {
        let _guard = env_guard();
        let config_home = temp_dir("registry-prune-home");
        let bundled_root = temp_dir("registry-prune-bundled");
        let install_root = config_home.join("plugins").join("installed");
        let missing_install_path = temp_dir("registry-prune-missing");

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(install_root);
        let manager = PluginManager::new(config);

        let mut registry = InstalledPluginRegistry::default();
        registry.plugins.insert(
            "stale-external@external".to_string(),
            InstalledPluginRecord {
                kind: PluginKind::External,
                id: "stale-external@external".to_string(),
                name: "stale-external".to_string(),
                version: "1.0.0".to_string(),
                description: "stale external plugin".to_string(),
                install_path: missing_install_path.clone(),
                source: PluginInstallSource::LocalPath {
                    path: missing_install_path.clone(),
                },
                installed_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            },
        );
        manager.store_registry(&registry).expect("store registry");

        let installed = manager
            .list_installed_plugins()
            .expect("stale registry entries should be pruned");
        assert!(!installed
            .iter()
            .any(|plugin| plugin.metadata.id == "stale-external@external"));

        let registry = manager.load_registry().expect("load registry");
        assert!(!registry.plugins.contains_key("stale-external@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn persists_bundled_plugin_enable_state_across_reloads() {
        let _guard = env_guard();
        let config_home = temp_dir("bundled-state-home");
        let bundled_root = temp_dir("bundled-state-root");
        write_bundled_plugin(&bundled_root.join("starter"), "starter", "0.1.0", false);

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let mut manager = PluginManager::new(config.clone());

        manager
            .enable("starter@bundled")
            .expect("enable bundled plugin should succeed");
        assert_eq!(
            load_enabled_plugins(&manager.settings_path()).get("starter@bundled"),
            Some(&true)
        );

        let mut reloaded_config = PluginManagerConfig::new(&config_home);
        reloaded_config.bundled_root = Some(bundled_root.clone());
        reloaded_config.enabled_plugins = load_enabled_plugins(&manager.settings_path());
        let reloaded_manager = PluginManager::new(reloaded_config);
        let reloaded = reloaded_manager
            .list_installed_plugins()
            .expect("bundled plugins should still be listed");
        assert!(reloaded
            .iter()
            .any(|plugin| { plugin.metadata.id == "starter@bundled" && plugin.enabled }));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn persists_bundled_plugin_disable_state_across_reloads() {
        let _guard = env_guard();
        let config_home = temp_dir("bundled-disabled-home");
        let bundled_root = temp_dir("bundled-disabled-root");
        write_bundled_plugin(&bundled_root.join("starter"), "starter", "0.1.0", true);

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let mut manager = PluginManager::new(config);

        manager
            .disable("starter@bundled")
            .expect("disable bundled plugin should succeed");
        assert_eq!(
            load_enabled_plugins(&manager.settings_path()).get("starter@bundled"),
            Some(&false)
        );

        let mut reloaded_config = PluginManagerConfig::new(&config_home);
        reloaded_config.bundled_root = Some(bundled_root.clone());
        reloaded_config.enabled_plugins = load_enabled_plugins(&manager.settings_path());
        let reloaded_manager = PluginManager::new(reloaded_config);
        let reloaded = reloaded_manager
            .list_installed_plugins()
            .expect("bundled plugins should still be listed");
        assert!(reloaded
            .iter()
            .any(|plugin| { plugin.metadata.id == "starter@bundled" && !plugin.enabled }));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn validates_plugin_source_before_install() {
        let _guard = env_guard();
        let config_home = temp_dir("validate-home");
        let source_root = temp_dir("validate-source");
        write_external_plugin(&source_root, "validator", "1.0.0");
        let manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let manifest = manager
            .validate_plugin_source(source_root.to_str().expect("utf8 path"))
            .expect("manifest should validate");
        assert_eq!(manifest.name, "validator");
        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn plugin_registry_tracks_enabled_state_and_lookup() {
        let _guard = env_guard();
        let config_home = temp_dir("registry-home");
        let source_root = temp_dir("registry-source");
        write_external_plugin(&source_root, "registry-demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect("install should succeed");
        manager
            .disable("registry-demo@external")
            .expect("disable should succeed");

        let registry = manager.plugin_registry().expect("registry should build");
        let plugin = registry
            .get("registry-demo@external")
            .expect("installed plugin should be discoverable");
        assert_eq!(plugin.metadata().name, "registry-demo");
        assert!(!plugin.is_enabled());
        assert!(registry.contains("registry-demo@external"));
        assert!(!registry.contains("missing@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn plugin_registry_report_collects_load_failures_without_dropping_valid_plugins() {
        let _guard = env_guard();
        // given
        let config_home = temp_dir("report-home");
        let external_root = temp_dir("report-external");
        write_external_plugin(&external_root.join("valid"), "valid-report", "1.0.0");
        write_broken_plugin(&external_root.join("broken"), "broken-report");

        let mut config = PluginManagerConfig::new(&config_home);
        config.external_dirs = vec![external_root.clone()];
        let manager = PluginManager::new(config);

        // when
        let report = manager
            .plugin_registry_report()
            .expect("report should tolerate invalid external plugins");

        // then
        assert!(report.registry().contains("valid-report@external"));
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].kind, PluginKind::External);
        assert!(report.failures()[0]
            .plugin_root
            .ends_with(Path::new("broken")));
        assert!(report.failures()[0]
            .error()
            .to_string()
            .contains("does not exist"));

        let error = manager
            .plugin_registry()
            .expect_err("strict registry should surface load failures");
        match error {
            PluginError::LoadFailures(failures) => {
                assert_eq!(failures.len(), 1);
                assert!(failures[0].plugin_root.ends_with(Path::new("broken")));
            }
            other => panic!("expected load failures, got {other}"),
        }

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(external_root);
    }

    #[test]
    fn installed_plugin_registry_report_collects_load_failures_from_install_root() {
        let _guard = env_guard();
        // given
        let config_home = temp_dir("installed-report-home");
        let bundled_root = temp_dir("installed-report-bundled");
        let install_root = config_home.join("plugins").join("installed");
        write_external_plugin(&install_root.join("valid"), "installed-valid", "1.0.0");
        write_broken_plugin(&install_root.join("broken"), "installed-broken");

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(install_root);
        let manager = PluginManager::new(config);

        // when
        let report = manager
            .installed_plugin_registry_report()
            .expect("installed report should tolerate invalid installed plugins");

        // then
        assert!(report.registry().contains("installed-valid@external"));
        assert_eq!(report.failures().len(), 1);
        assert!(report.failures()[0]
            .plugin_root
            .ends_with(Path::new("broken")));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn rejects_plugin_sources_with_missing_hook_paths() {
        let _guard = env_guard();
        // given
        let config_home = temp_dir("broken-home");
        let source_root = temp_dir("broken-source");
        write_broken_plugin(&source_root, "broken");

        let manager = PluginManager::new(PluginManagerConfig::new(&config_home));

        // when
        let error = manager
            .validate_plugin_source(source_root.to_str().expect("utf8 path"))
            .expect_err("missing hook file should fail validation");

        // then
        assert!(error.to_string().contains("does not exist"));

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install_error = manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect_err("install should reject invalid hook paths");
        assert!(install_error.to_string().contains("does not exist"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn rejects_plugin_sources_with_missing_failure_hook_paths() {
        let _guard = env_guard();
        // given
        let config_home = temp_dir("broken-failure-home");
        let source_root = temp_dir("broken-failure-source");
        write_broken_failure_hook_plugin(&source_root, "broken-failure");

        let manager = PluginManager::new(PluginManagerConfig::new(&config_home));

        // when
        let error = manager
            .validate_plugin_source(source_root.to_str().expect("utf8 path"))
            .expect_err("missing failure hook file should fail validation");

        // then
        assert!(error.to_string().contains("does not exist"));

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install_error = manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect_err("install should reject invalid failure hook paths");
        assert!(install_error.to_string().contains("does not exist"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn plugin_registry_runs_initialize_and_shutdown_for_enabled_plugins() {
        let _guard = env_guard();
        let config_home = temp_dir("lifecycle-home");
        let source_root = temp_dir("lifecycle-source");
        let _ = write_lifecycle_plugin(&source_root, "lifecycle-demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install = manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect("install should succeed");
        let log_path = install.install_path.join("lifecycle.log");

        let registry = manager.plugin_registry().expect("registry should build");
        registry.initialize().expect("init should succeed");
        registry.shutdown().expect("shutdown should succeed");

        let log = fs::read_to_string(&log_path).expect("lifecycle log should exist");
        assert_eq!(log, "init\nshutdown\n");

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn aggregates_and_executes_plugin_tools() {
        let _guard = env_guard();
        let config_home = temp_dir("tool-home");
        let source_root = temp_dir("tool-source");
        write_tool_plugin(&source_root, "tool-demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        manager
            .install(source_root.to_str().expect("utf8 path"))
            .expect("install should succeed");

        let tools = manager.aggregated_tools().expect("tools should aggregate");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition().name, "plugin_echo");
        assert_eq!(tools[0].required_permission(), "workspace-write");

        let output = tools[0]
            .execute(&serde_json::json!({ "message": "hello" }))
            .expect("plugin tool should execute");
        let payload: Value = serde_json::from_str(&output).expect("valid json");
        assert_eq!(payload["plugin"], "tool-demo@external");
        assert_eq!(payload["tool"], "plugin_echo");
        assert_eq!(payload["input"]["message"], "hello");

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn list_installed_plugins_scans_install_root_without_registry_entries() {
        let _guard = env_guard();
        let config_home = temp_dir("installed-scan-home");
        let bundled_root = temp_dir("installed-scan-bundled");
        let install_root = config_home.join("plugins").join("installed");
        let installed_plugin_root = install_root.join("scan-demo");
        write_file(
            installed_plugin_root.join(MANIFEST_FILE_NAME).as_path(),
            r#"{
  "name": "scan-demo",
  "version": "1.0.0",
  "description": "Scanned from install root"
}"#,
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(install_root);
        let manager = PluginManager::new(config);

        let installed = manager
            .list_installed_plugins()
            .expect("installed plugins should scan directories");
        assert!(installed
            .iter()
            .any(|plugin| plugin.metadata.id == "scan-demo@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn list_installed_plugins_scans_packaged_manifests_in_install_root() {
        let _guard = env_guard();
        let config_home = temp_dir("installed-packaged-scan-home");
        let bundled_root = temp_dir("installed-packaged-scan-bundled");
        let install_root = config_home.join("plugins").join("installed");
        let installed_plugin_root = install_root.join("scan-packaged");
        write_file(
            installed_plugin_root.join(MANIFEST_RELATIVE_PATH).as_path(),
            r#"{
  "name": "scan-packaged",
  "version": "1.0.0",
  "description": "Packaged manifest in install root"
}"#,
        );

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        config.install_root = Some(install_root);
        let manager = PluginManager::new(config);

        let installed = manager
            .list_installed_plugins()
            .expect("installed plugins should scan packaged manifests");
        assert!(installed
            .iter()
            .any(|plugin| plugin.metadata.id == "scan-packaged@external"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    /// Regression test for ROADMAP #41: verify that `CLAW_CONFIG_HOME` isolation prevents
    /// host `~/.claw/plugins/` from bleeding into test runs.
    #[test]
    fn claw_config_home_isolation_prevents_host_plugin_leakage() {
        let _guard = env_guard();

        // Create a temp directory to act as our isolated CLAW_CONFIG_HOME
        let config_home = temp_dir("isolated-home");
        let bundled_root = temp_dir("isolated-bundled");

        // Set CLAW_CONFIG_HOME to our temp directory
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);

        // Create a test fixture plugin in the isolated config home
        let install_root = config_home.join("plugins").join("installed");
        let fixture_plugin_root = install_root.join("isolated-test-plugin");
        write_file(
            fixture_plugin_root.join(MANIFEST_RELATIVE_PATH).as_path(),
            r#"{
  "name": "isolated-test-plugin",
  "version": "1.0.0",
  "description": "Test fixture plugin in isolated config home"
}"#,
        );

        // Create PluginManager with isolated bundled_root - it should use the temp config_home, not host ~/.claw/
        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let manager = PluginManager::new(config);

        // List installed plugins - should only see the test fixture, not host plugins
        let installed = manager
            .list_installed_plugins()
            .expect("installed plugins should list");

        // Verify we only see the test fixture plugin
        assert_eq!(
            installed.len(),
            1,
            "should only see the test fixture plugin, not host ~/.claw/plugins/"
        );
        assert_eq!(
            installed[0].metadata.id, "isolated-test-plugin@external",
            "should see the test fixture plugin"
        );

        // Cleanup
        std::env::remove_var("CLAW_CONFIG_HOME");
        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn plugin_lifecycle_handles_parallel_execution() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Arc;
        use std::thread;

        let _guard = env_guard();

        // Shared base directory for all threads
        let base_dir = temp_dir("parallel-base");

        // Track successful installations and any errors
        let success_count = Arc::new(AtomicUsize::new(0));
        let error_count = Arc::new(AtomicUsize::new(0));

        // Spawn multiple threads to install plugins simultaneously
        let mut handles = Vec::new();
        for thread_id in 0..5 {
            let base_dir = base_dir.clone();
            let success_count = Arc::clone(&success_count);
            let error_count = Arc::clone(&error_count);

            let handle = thread::spawn(move || {
                // Create unique directories for this thread
                let config_home = base_dir.join(format!("config-{thread_id}"));
                let source_root = base_dir.join(format!("source-{thread_id}"));

                // Write lifecycle plugin for this thread
                let _log_path =
                    write_lifecycle_plugin(&source_root, &format!("parallel-{thread_id}"), "1.0.0");

                // Create PluginManager and install
                let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
                let install_result = manager.install(source_root.to_str().expect("utf8 path"));

                match install_result {
                    Ok(install) => {
                        let log_path = install.install_path.join("lifecycle.log");

                        // Initialize and shutdown the registry to trigger lifecycle hooks
                        let registry = manager.plugin_registry();
                        match registry {
                            Ok(registry) => {
                                if registry.initialize().is_ok() && registry.shutdown().is_ok() {
                                    // Verify lifecycle.log exists and has expected content
                                    if let Ok(log) = fs::read_to_string(&log_path) {
                                        if log == "init\nshutdown\n" {
                                            success_count.fetch_add(1, AtomicOrdering::Relaxed);
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                error_count.fetch_add(1, AtomicOrdering::Relaxed);
                            }
                        }
                    }
                    Err(_) => {
                        error_count.fetch_add(1, AtomicOrdering::Relaxed);
                    }
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("thread should complete");
        }

        // Verify all threads succeeded without collisions
        let successes = success_count.load(AtomicOrdering::Relaxed);
        let errors = error_count.load(AtomicOrdering::Relaxed);

        assert_eq!(
            successes, 5,
            "all 5 parallel plugin installations should succeed"
        );
        assert_eq!(
            errors, 0,
            "no errors should occur during parallel execution"
        );

        // Cleanup
        let _ = fs::remove_dir_all(base_dir);
    }
}
