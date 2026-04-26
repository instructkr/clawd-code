//! Extension system for registering tools, commands, and lifecycle hooks
//! at runtime.

use crate::tool_registry::ToolRegistry;

/// An extension can register tools and respond to lifecycle events.
pub trait Extension: std::fmt::Debug + Send {
    /// The name of this extension.
    fn name(&self) -> &str;

    /// Register tools with the given registry.
    fn register_tools(&self, registry: &mut ToolRegistry);

    /// Called when a turn starts.
    fn on_turn_start(&self) {}

    /// Called when a turn completes.
    fn on_turn_complete(&self) {}

    /// Called when an error occurs.
    fn on_error(&self, _message: &str) {}
}

/// Registry of loaded extensions.
#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    extensions: Vec<Box<dyn Extension>>,
}

impl ExtensionRegistry {
    /// Create a new empty extension registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            extensions: Vec::new(),
        }
    }

    /// Register an extension.
    pub fn register(&mut self, extension: Box<dyn Extension>) {
        self.extensions.push(extension);
    }

    /// Get all registered extensions.
    #[must_use]
    pub fn extensions(&self) -> &[Box<dyn Extension>] {
        &self.extensions
    }

    /// Collect all tool names from all registered extensions.
    pub fn collect_tools(&self, registry: &mut ToolRegistry) {
        for ext in &self.extensions {
            ext.register_tools(registry);
        }
    }

    /// Notify all extensions that a turn started.
    pub fn notify_turn_start(&self) {
        for ext in &self.extensions {
            ext.on_turn_start();
        }
    }

    /// Notify all extensions that a turn completed.
    pub fn notify_turn_complete(&self) {
        for ext in &self.extensions {
            ext.on_turn_complete();
        }
    }

    /// Notify all extensions of an error.
    pub fn notify_error(&self, message: &str) {
        for ext in &self.extensions {
            ext.on_error(message);
        }
    }
}

/// A simple extension that just registers tools.
#[derive(Debug)]
pub struct SimpleExtension {
    name: String,
    tools: Vec<String>,
}

impl SimpleExtension {
    /// Create a new simple extension.
    #[must_use]
    pub fn new(name: &str, tools: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            tools,
        }
    }
}

impl Extension for SimpleExtension {
    fn name(&self) -> &str {
        &self.name
    }

    fn register_tools(&self, registry: &mut ToolRegistry) {
        for tool in &self.tools {
            registry.register_builtin(tool);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::create_builtin_tools;

    #[test]
    fn extension_registry_collects_tools() {
        let mut registry = create_builtin_tools();
        let mut ext_registry = ExtensionRegistry::new();

        ext_registry.register(Box::new(SimpleExtension::new(
            "my-ext",
            vec!["my_custom_tool".to_string()],
        )));

        ext_registry.collect_tools(&mut registry);
        assert!(registry.has_tool("my_custom_tool"));
    }

    #[test]
    fn extension_lifecycle_notifications() {
        let mut ext_registry = ExtensionRegistry::new();
        ext_registry.register(Box::new(SimpleExtension::new("test", vec![])));

        // Should not panic
        ext_registry.notify_turn_start();
        ext_registry.notify_turn_complete();
        ext_registry.notify_error("test error");
    }
}
