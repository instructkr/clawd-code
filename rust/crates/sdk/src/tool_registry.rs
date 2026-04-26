use std::collections::BTreeMap;
use std::fmt;

use runtime::{ToolError, ToolExecutor};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Tool handler trait
// ---------------------------------------------------------------------------

/// A handler that executes a custom tool invocation.
///
/// Implement this trait to provide custom tool logic. The SDK's
/// `SdkToolExecutor` dispatches to registered handlers when a model
/// requests a tool call.
pub trait ToolHandler: Send + Sync {
    /// Execute the tool with the given JSON input and return JSON output.
    fn execute(&self, input: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// Schema validation
// ---------------------------------------------------------------------------

/// Minimal JSON Schema validation for tool input/output.
///
/// Validates required fields and basic type constraints. This is intentionally
/// lightweight — for full JSON Schema compliance, integrate a dedicated crate.
#[derive(Debug, Clone)]
pub struct SchemaValidator {
    schema: Value,
}

impl SchemaValidator {
    /// Create a validator from a JSON Schema value.
    #[must_use]
    pub fn new(schema: Value) -> Self {
        Self { schema }
    }

    /// Create a no-op validator that passes everything.
    #[must_use]
    pub fn none() -> Self {
        Self {
            schema: Value::Object(serde_json::Map::new()),
        }
    }

    /// Validate a JSON value against this schema.
    ///
    /// Checks:
    /// - `type` assertion (string, number, boolean, object, array)
    /// - `required` fields (for objects)
    /// - `properties` existence (for objects)
    pub fn validate(&self, value: &Value) -> Result<(), SchemaValidationError> {
        let obj = match self.schema.as_object() {
            Some(o) => o,
            None => return Ok(()), // No schema constraints
        };

        // Check type
        if let Some(schema_type) = obj.get("type").and_then(|t| t.as_str()) {
            let type_matches = match schema_type {
                "integer" => value.as_i64().is_some(),
                _ => json_type_of(value) == schema_type,
            };
            if !type_matches {
                return Err(SchemaValidationError {
                    path: String::new(),
                    expected: schema_type.to_string(),
                    actual: json_type_of(value).to_string(),
                });
            }
        }

        // Check required fields (for objects)
        if let Some(required) = obj.get("required").and_then(|r| r.as_array()) {
            if let Some(value_obj) = value.as_object() {
                for field in required {
                    if let Some(field_name) = field.as_str() {
                        if !value_obj.contains_key(field_name) {
                            return Err(SchemaValidationError {
                                path: field_name.to_string(),
                                expected: "required field present".to_string(),
                                actual: "field missing".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Recurse into properties
        if let (Some(properties), Some(value_obj)) =
            (obj.get("properties").and_then(|p| p.as_object()), value.as_object())
        {
            for (key, prop_schema) in properties {
                if let Some(child_value) = value_obj.get(key) {
                    let child_validator = SchemaValidator::new(prop_schema.clone());
                    child_validator.validate(child_value).map_err(|mut e| {
                        e.path = if e.path.is_empty() {
                            key.clone()
                        } else {
                            format!("{key}.{}", e.path)
                        };
                        e
                    })?;
                }
            }
        }

        Ok(())
    }

    /// Get the underlying schema.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }
}

impl Default for SchemaValidator {
    fn default() -> Self {
        Self::none()
    }
}

/// Error returned when schema validation fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaValidationError {
    /// JSON path to the failing field.
    pub path: String,
    /// What was expected.
    pub expected: String,
    /// What was actually found.
    pub actual: String,
}

impl fmt::Display for SchemaValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "validation error: expected {}, got {}", self.expected, self.actual)
        } else {
            write!(
                f,
                "validation error at '{}': expected {}, got {}",
                self.path, self.expected, self.actual
            )
        }
    }
}

fn json_type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

/// A fully-defined tool with schema, description, and optional handler.
#[derive(Clone)]
pub struct ToolDefinition {
    /// Tool name (must be unique within the registry).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema for the tool's input.
    pub input_schema: SchemaValidator,
    /// JSON Schema for the tool's output (optional).
    pub output_schema: SchemaValidator,
    /// Custom handler. If `None`, the tool is a builtin stub.
    handler: Option<Arc<dyn ToolHandler>>,
}

impl ToolDefinition {
    /// Start building a new tool definition.
    #[must_use]
    pub fn builder(name: &str) -> ToolDefinitionBuilder {
        ToolDefinitionBuilder {
            name: name.to_string(),
            description: String::new(),
            input_schema: None,
            output_schema: None,
            handler: None,
        }
    }
}

impl fmt::Debug for ToolDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish()
    }
}

use std::sync::Arc;

// ---------------------------------------------------------------------------
// define_tool() ergonomic builder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing [`ToolDefinition`] instances.
pub struct ToolDefinitionBuilder {
    name: String,
    description: String,
    input_schema: Option<SchemaValidator>,
    output_schema: Option<SchemaValidator>,
    handler: Option<Arc<dyn ToolHandler>>,
}

impl ToolDefinitionBuilder {
    /// Set the tool description.
    #[must_use]
    pub fn description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    /// Set the input JSON Schema.
    #[must_use]
    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = Some(SchemaValidator::new(schema));
        self
    }

    /// Set the output JSON Schema.
    #[must_use]
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(SchemaValidator::new(schema));
        self
    }

    /// Provide a custom handler for tool execution.
    pub fn handler(mut self, handler: impl ToolHandler + 'static) -> Self {
        self.handler = Some(Arc::new(handler));
        self
    }

    /// Build the tool definition.
    ///
    /// Returns an error if the tool name is empty.
    pub fn build(self) -> Result<ToolDefinition, String> {
        if self.name.is_empty() {
            return Err("tool name cannot be empty".to_string());
        }
        Ok(ToolDefinition {
            name: self.name,
            description: self.description,
            input_schema: self.input_schema.unwrap_or_default(),
            output_schema: self.output_schema.unwrap_or_default(),
            handler: self.handler,
        })
    }
}

/// Convenience function: create a tool definition builder.
///
/// ```rust
/// use sdk::define_tool;
/// use serde_json::json;
///
/// let tool = define_tool("my_tool")
///     .description("Does something useful")
///     .input_schema(json!({
///         "type": "object",
///         "required": ["path"],
///         "properties": {
///             "path": {"type": "string"}
///         }
///     }))
///     .build()
///     .expect("should build");
/// ```
pub fn define_tool(name: &str) -> ToolDefinitionBuilder {
    ToolDefinition::builder(name)
}

// ---------------------------------------------------------------------------
// Closure-based ToolHandler
// ---------------------------------------------------------------------------

/// Wrap a plain `Fn(&str) -> Result<String, ToolError>` as a `ToolHandler`.
pub struct FnToolHandler {
    f: Box<dyn Fn(&str) -> Result<String, ToolError> + Send + Sync>,
}

impl FnToolHandler {
    pub fn new(
        f: impl Fn(&str) -> Result<String, ToolError> + Send + Sync + 'static,
    ) -> Self {
        Self { f: Box::new(f) }
    }
}

impl ToolHandler for FnToolHandler {
    fn execute(&self, input: &str) -> Result<String, ToolError> {
        (self.f)(input)
    }
}

// ---------------------------------------------------------------------------
// Tool registry (enhanced)
// ---------------------------------------------------------------------------

/// A registry of tools for use with the SDK.
///
/// Supports both built-in tool names (stubs) and fully-defined custom tools
/// with handlers and JSON Schema validation.
#[derive(Default)]
pub struct ToolRegistry {
    /// Ordered list of tool names (for iteration in insertion order).
    tool_names: Vec<String>,
    /// Descriptions for built-in tools.
    descriptions: BTreeMap<String, String>,
    /// Fully-defined custom tools with schema and handlers.
    definitions: BTreeMap<String, ToolDefinition>,
}

impl ToolRegistry {
    /// Create a new empty tool registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a built-in tool by name. Built-in tools are stubs — they have
    /// no handler and will return an error when executed.
    pub fn register_builtin(&mut self, name: &str) {
        if !self.tool_names.contains(&name.to_string()) {
            self.tool_names.push(name.to_string());
            self.descriptions
                .insert(name.to_string(), format!("built-in tool: {name}"));
        }
    }

    /// Register a fully-defined custom tool with optional handler and schemas.
    ///
    /// Returns an error if a tool with the same name is already registered.
    pub fn register(&mut self, def: ToolDefinition) -> Result<(), String> {
        if self.has_tool(&def.name) {
            return Err(format!("tool '{}' is already registered", def.name));
        }
        self.tool_names.push(def.name.clone());
        self.descriptions
            .insert(def.name.clone(), def.description.clone());
        self.definitions.insert(def.name.clone(), def);
        Ok(())
    }

    /// Check if a tool is registered.
    #[must_use]
    pub fn has_tool(&self, name: &str) -> bool {
        self.tool_names.iter().any(|n| n == name)
    }

    /// Get all registered tool names.
    #[must_use]
    pub fn tool_names(&self) -> &[String] {
        &self.tool_names
    }

    /// Get the description for a tool.
    #[must_use]
    pub fn description(&self, name: &str) -> Option<&str> {
        self.descriptions.get(name).map(String::as_str)
    }

    /// Get a registered tool definition.
    #[must_use]
    pub fn get_definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.definitions.get(name)
    }

    /// Validate tool input against the registered schema.
    pub fn validate_input(&self, name: &str, input: &Value) -> Result<(), SchemaValidationError> {
        match self.definitions.get(name) {
            Some(def) => def.input_schema.validate(input),
            None => Ok(()), // Built-in tools have no schema in the registry
        }
    }

    /// Validate tool output against the registered schema.
    pub fn validate_output(&self, name: &str, output: &Value) -> Result<(), SchemaValidationError> {
        match self.definitions.get(name) {
            Some(def) => def.output_schema.validate(output),
            None => Ok(()),
        }
    }
}

/// Create a default set of built-in tools for SDK usage.
#[must_use]
pub fn create_builtin_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for name in &[
        "read_file",
        "write_file",
        "edit_file",
        "glob_search",
        "grep_search",
        "bash",
        "WebFetch",
        "WebSearch",
        "TodoWrite",
        "Agent",
    ] {
        registry.register_builtin(name);
    }
    registry
}

// ---------------------------------------------------------------------------
// SdkToolExecutor (enhanced)
// ---------------------------------------------------------------------------

/// Tool executor for SDK sessions.
///
/// Dispatches to registered custom tool handlers with schema validation.
/// Built-in tools that have no registered handler return a stub error.
pub struct SdkToolExecutor {
    /// Known tool names (for existence checks).
    tool_names: BTreeMap<String, ()>,
    /// Custom tool definitions with handlers.
    definitions: BTreeMap<String, ToolDefinition>,
}

impl SdkToolExecutor {
    /// Create an executor from the current state of a `ToolRegistry`.
    #[must_use]
    pub fn new(tools: &ToolRegistry) -> Self {
        let mut tool_names = BTreeMap::new();
        let mut definitions = BTreeMap::new();

        for name in tools.tool_names() {
            tool_names.insert(name.clone(), ());
        }
        for (name, def) in &tools.definitions {
            definitions.insert(name.clone(), def.clone());
        }

        Self {
            tool_names,
            definitions,
        }
    }
}

impl ToolExecutor for SdkToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        // Check if tool exists
        if !self.tool_names.contains_key(tool_name) {
            return Err(ToolError::new(format!("unknown tool: {tool_name}")));
        }

        // Try custom handler first
        if let Some(def) = self.definitions.get(tool_name) {
            // Validate input against schema
            let parsed: Value = match serde_json::from_str(input) {
                Ok(v) => v,
                Err(_) => {
                    // If the tool has a non-trivial input schema, reject malformed JSON
                    if def.input_schema.schema() != &Value::Object(serde_json::Map::new()) {
                        return Err(ToolError::new(format!(
                            "input validation failed for '{tool_name}': input is not valid JSON"
                        )));
                    }
                    // No schema constraints — let the handler deal with raw input
                    Value::Null
                }
            };
            if let Err(e) = def.input_schema.validate(&parsed) {
                return Err(ToolError::new(format!(
                    "input validation failed for '{tool_name}': {e}"
                )));
            }

            // Dispatch to handler
            if let Some(handler) = &def.handler {
                let result = handler.execute(input)?;

                // Validate output against schema
                if let Ok(parsed_output) = serde_json::from_str::<Value>(&result) {
                    if let Err(e) = def.output_schema.validate(&parsed_output) {
                        return Err(ToolError::new(format!(
                            "output validation failed for '{tool_name}': {e}"
                        )));
                    }
                }

                return Ok(result);
            }
        }

        // Built-in stub — no handler registered
        Err(ToolError::new(format!(
            "SDK stub: {tool_name} called with {input} — \
             provide a custom ToolHandler via define_tool().handler()"
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- SchemaValidator ---

    #[test]
    fn validates_required_fields() {
        let validator = SchemaValidator::new(json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"}
            }
        }));

        assert!(validator.validate(&json!({"path": "/tmp/f"})).is_ok());
        assert!(validator.validate(&json!({"other": 1})).is_err());
    }

    #[test]
    fn validates_type_constraints() {
        let validator = SchemaValidator::new(json!({"type": "string"}));
        assert!(validator.validate(&json!("hello")).is_ok());
        assert!(validator.validate(&json!(42)).is_err());
    }

    #[test]
    fn validates_nested_properties() {
        let validator = SchemaValidator::new(json!({
            "type": "object",
            "properties": {
                "config": {
                    "type": "object",
                    "required": ["enabled"],
                    "properties": {
                        "enabled": {"type": "boolean"}
                    }
                }
            }
        }));

        assert!(validator.validate(&json!({"config": {"enabled": true}})).is_ok());
        assert!(validator.validate(&json!({"config": {"enabled": "yes"}})).is_err());
        assert!(validator.validate(&json!({"config": {}})).is_err());
    }

    #[test]
    fn none_validator_passes_everything() {
        let validator = SchemaValidator::none();
        assert!(validator.validate(&json!(null)).is_ok());
        assert!(validator.validate(&json!(42)).is_ok());
        assert!(validator.validate(&json!("anything")).is_ok());
    }

    // --- define_tool builder ---

    #[test]
    fn define_tool_creates_valid_definition() {
        let tool = define_tool("my_tool")
            .description("Does something")
            .input_schema(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"}
                }
            }))
            .build()
            .expect("should build");

        assert_eq!(tool.name, "my_tool");
        assert_eq!(tool.description, "Does something");
    }

    #[test]
    fn define_tool_rejects_empty_name() {
        let result = define_tool("").build();
        assert!(result.is_err());
    }

    // --- ToolRegistry ---

    #[test]
    fn tool_registry_manages_tool_names() {
        let mut registry = ToolRegistry::new();
        registry.register_builtin("read_file");
        registry.register_builtin("bash");

        assert!(registry.has_tool("read_file"));
        assert!(registry.has_tool("bash"));
        assert!(!registry.has_tool("nonexistent"));
        assert_eq!(registry.tool_names().len(), 2);
    }

    #[test]
    fn create_builtin_tools_includes_standard_tools() {
        let registry = create_builtin_tools();
        assert!(registry.has_tool("read_file"));
        assert!(registry.has_tool("bash"));
        assert!(registry.has_tool("Agent"));
        assert!(registry.tool_names().len() >= 10);
    }

    #[test]
    fn register_custom_tool_with_handler() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("custom")
            .description("Custom tool")
            .input_schema(json!({"type": "object", "required": ["x"]}))
            .handler(FnToolHandler::new(|_input| Ok("result".to_string())))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        assert!(registry.has_tool("custom"));
        assert_eq!(registry.description("custom"), Some("Custom tool"));
    }

    #[test]
    fn reject_duplicate_tool_registration() {
        let mut registry = ToolRegistry::new();
        registry.register_builtin("bash");

        let tool = define_tool("bash")
            .description("Custom bash")
            .build()
            .expect("should build");

        let result = registry.register(tool);
        assert!(result.is_err());
    }

    #[test]
    fn validate_input_via_registry() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("check")
            .input_schema(json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {"type": "string"}
                }
            }))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");

        assert!(registry.validate_input("check", &json!({"url": "http://x"})).is_ok());
        assert!(registry.validate_input("check", &json!({"x": 1})).is_err());
    }

    // --- SdkToolExecutor with handler dispatch ---

    #[test]
    fn executor_dispatches_to_custom_handler() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("echo")
            .handler(FnToolHandler::new(|input| {
                let parsed: Value = serde_json::from_str(input).unwrap();
                let msg = parsed["message"].as_str().unwrap();
                Ok(json!({"echo": msg}).to_string())
            }))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("echo", r#"{"message": "hello"}"#);
        assert!(result.is_ok());
        let output: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output["echo"], "hello");
    }

    #[test]
    fn executor_rejects_invalid_input() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("strict")
            .input_schema(json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {"type": "string"}
                }
            }))
            .handler(FnToolHandler::new(|_| Ok("ok".to_string())))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("strict", r#"{"age": 5}"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("input validation failed"),
            "should mention validation: {err}"
        );
    }

    #[test]
    fn executor_returns_unknown_tool_error() {
        let registry = ToolRegistry::new();
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("nope", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }

    #[test]
    fn executor_returns_stub_for_builtin_without_handler() {
        let mut registry = ToolRegistry::new();
        registry.register_builtin("bash");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("bash", "ls");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SDK stub"));
    }

    #[test]
    fn executor_validates_output() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("bounded")
            .output_schema(json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": {"type": "string"}
                }
            }))
            .handler(FnToolHandler::new(|_| Ok("not json".to_string())))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        // Non-JSON output bypasses output validation (handler result is not valid JSON)
        let result = exec.execute("bounded", "{}");
        assert!(result.is_ok()); // Passes because output isn't parseable as JSON
    }

    #[test]
    fn executor_validates_output_on_json_return() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("bad_output")
            .output_schema(json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": {"type": "string"}
                }
            }))
            .handler(FnToolHandler::new(|_| {
                Ok(json!({"count": 42}).to_string()) // missing "status"
            }))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("bad_output", "{}");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("output validation failed"),
            "should mention output validation: {err}"
        );
    }

    // --- Additional coverage: SchemaValidator edge cases ---

    #[test]
    fn validates_integer_type() {
        let validator = SchemaValidator::new(json!({"type": "integer"}));
        assert!(validator.validate(&json!(42)).is_ok());
        assert!(validator.validate(&json!(0)).is_ok());
        assert!(validator.validate(&json!(-1)).is_ok());
        assert!(validator.validate(&json!(3.14)).is_err(), "float should fail integer check");
        assert!(validator.validate(&json!("42")).is_err(), "string should fail integer check");
    }

    #[test]
    fn validates_null_type() {
        let validator = SchemaValidator::new(json!({"type": "null"}));
        assert!(validator.validate(&json!(null)).is_ok());
        assert!(validator.validate(&json!(0)).is_err());
    }

    #[test]
    fn validates_array_type() {
        let validator = SchemaValidator::new(json!({"type": "array"}));
        assert!(validator.validate(&json!([1, 2, 3])).is_ok());
        assert!(validator.validate(&json!("not array")).is_err());
    }

    #[test]
    fn validates_boolean_type() {
        let validator = SchemaValidator::new(json!({"type": "boolean"}));
        assert!(validator.validate(&json!(true)).is_ok());
        assert!(validator.validate(&json!(false)).is_ok());
        assert!(validator.validate(&json!(1)).is_err());
    }

    #[test]
    fn validates_number_type() {
        let validator = SchemaValidator::new(json!({"type": "number"}));
        assert!(validator.validate(&json!(42)).is_ok());
        assert!(validator.validate(&json!(3.14)).is_ok());
        assert!(validator.validate(&json!("42")).is_err());
    }

    #[test]
    fn empty_schema_object_passes_everything() {
        let validator = SchemaValidator::new(json!({}));
        assert!(validator.validate(&json!(null)).is_ok());
        assert!(validator.validate(&json!(42)).is_ok());
        assert!(validator.validate(&json!("str")).is_ok());
        assert!(validator.validate(&json!([1, 2])).is_ok());
        assert!(validator.validate(&json!({"a": 1})).is_ok());
    }

    #[test]
    fn non_object_schema_passes_everything() {
        // A schema that is not an object (e.g. a string) has no constraints
        let validator = SchemaValidator::new(json!("not an object"));
        assert!(validator.validate(&json!(42)).is_ok());
    }

    #[test]
    fn validates_deeply_nested_properties() {
        let validator = SchemaValidator::new(json!({
            "type": "object",
            "properties": {
                "level1": {
                    "type": "object",
                    "properties": {
                        "level2": {
                            "type": "object",
                            "properties": {
                                "level3": {"type": "string"}
                            }
                        }
                    }
                }
            }
        }));

        assert!(validator.validate(&json!({"level1": {"level2": {"level3": "ok"}}})).is_ok());
        let err = validator.validate(&json!({"level1": {"level2": {"level3": 42}}}));
        assert!(err.is_err());
        let err = err.unwrap_err();
        assert!(
            err.path.contains("level3"),
            "path should contain level3: got '{}'",
            err.path
        );
    }

    // --- Additional coverage: Executor edge cases ---

    #[test]
    fn executor_rejects_malformed_json_with_schema() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("needs_schema")
            .input_schema(json!({"type": "object", "required": ["x"]}))
            .handler(FnToolHandler::new(|_| Ok("ok".to_string())))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("needs_schema", "{invalid json");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not valid JSON"),
            "should mention invalid JSON: {err}"
        );
    }

    #[test]
    fn executor_handler_error_propagates() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("failing")
            .handler(FnToolHandler::new(|_| {
                Err(ToolError::new("handler exploded"))
            }))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("failing", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("handler exploded"));
    }

    #[test]
    fn executor_empty_tool_name_returns_unknown() {
        let registry = ToolRegistry::new();
        let mut exec = SdkToolExecutor::new(&registry);

        let result = exec.execute("", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }

    #[test]
    fn executor_custom_tool_without_handler_returns_stub() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("no_handler").build().expect("should build");
        registry.register(tool).expect("should register");

        let mut exec = SdkToolExecutor::new(&registry);
        let result = exec.execute("no_handler", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SDK stub"));
    }

    // --- Additional coverage: Registry edge cases ---

    #[test]
    fn register_builtin_is_idempotent() {
        let mut registry = ToolRegistry::new();
        registry.register_builtin("bash");
        registry.register_builtin("bash");
        assert_eq!(registry.tool_names().len(), 1);
    }

    #[test]
    fn register_builtin_after_custom_same_name_does_not_duplicate() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("bash")
            .description("Custom bash")
            .build()
            .expect("should build");
        registry.register(tool).expect("should register");

        // register_builtin does not check definitions map, but has_tool checks tool_names
        // Since the name is already in tool_names, it won't add a duplicate
        registry.register_builtin("bash");
        assert_eq!(registry.tool_names().len(), 1);
        assert_eq!(registry.description("bash"), Some("Custom bash"));
    }

    #[test]
    fn validate_input_for_unregistered_tool_passes() {
        let registry = ToolRegistry::new();
        // Unregistered tool has no schema, so validation is a no-op
        assert!(registry.validate_input("nonexistent", &json!({"x": 1})).is_ok());
    }

    #[test]
    fn get_definition_returns_none_for_builtin() {
        let mut registry = ToolRegistry::new();
        registry.register_builtin("bash");
        assert!(registry.get_definition("bash").is_none());
    }

    #[test]
    fn minimal_tool_definition_builds() {
        let tool = define_tool("minimal").build().expect("should build");
        assert_eq!(tool.name, "minimal");
        assert_eq!(tool.description, "");
        assert!(tool.input_schema.validate(&json!({})).is_ok());
    }

    // --- End-to-end integration ---

    #[test]
    fn full_pipeline_define_register_execute_with_validation() {
        let mut registry = ToolRegistry::new();

        let tool = define_tool("transform")
            .description("Transforms input")
            .input_schema(json!({
                "type": "object",
                "required": ["value"],
                "properties": {
                    "value": {"type": "integer"}
                }
            }))
            .output_schema(json!({
                "type": "object",
                "required": ["doubled"],
                "properties": {
                    "doubled": {"type": "number"}
                }
            }))
            .handler(FnToolHandler::new(|input| {
                let parsed: Value = serde_json::from_str(input).unwrap();
                let val = parsed["value"].as_i64().unwrap();
                Ok(json!({"doubled": val * 2}).to_string())
            }))
            .build()
            .expect("should build");

        registry.register(tool).expect("should register");

        // Validate input directly via registry
        assert!(registry.validate_input("transform", &json!({"value": 5})).is_ok());
        assert!(registry.validate_input("transform", &json!({"value": "not int"})).is_err());

        // Execute via executor
        let mut exec = SdkToolExecutor::new(&registry);
        let result = exec.execute("transform", r#"{"value": 7}"#);
        assert!(result.is_ok());
        let output: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output["doubled"], 14);
    }

    #[test]
    fn schema_validation_error_display_both_branches() {
        let with_path = SchemaValidationError {
            path: "field".to_string(),
            expected: "string".to_string(),
            actual: "number".to_string(),
        };
        let msg = with_path.to_string();
        assert!(msg.contains("at 'field'"), "should contain path: {msg}");

        let without_path = SchemaValidationError {
            path: String::new(),
            expected: "string".to_string(),
            actual: "number".to_string(),
        };
        let msg = without_path.to_string();
        assert!(!msg.contains("at ''"), "should not contain empty path: {msg}");
    }
}
