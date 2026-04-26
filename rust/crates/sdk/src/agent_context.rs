use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// A thread-safe shared context store for inter-agent communication.
///
/// Multiple agents can read and write to this store to share state
/// during a multi-agent workflow. The store is keyed by string and
/// can hold any serializable value as a JSON string.
#[derive(Debug, Clone)]
pub struct AgentContext {
    inner: Arc<RwLock<BTreeMap<String, String>>>,
}

impl AgentContext {
    /// Create a new empty shared context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Set a value in the context.
    pub fn set(&self, key: &str, value: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.insert(key.to_string(), value.to_string());
        }
    }

    /// Get a value from the context.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        self.inner.read().ok().and_then(|map| map.get(key).cloned())
    }

    /// Remove a value from the context.
    pub fn remove(&self, key: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.remove(key);
        }
    }

    /// Check if a key exists.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.inner.read().is_ok_and(|map| map.contains_key(key))
    }

    /// Get all keys.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.inner
            .read()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Get the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().map_or(0, |map| map.len())
    }

    /// Check if the context is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all values.
    pub fn clear(&self) {
        if let Ok(mut map) = self.inner.write() {
            map.clear();
        }
    }
}

impl Default for AgentContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Represents a sub-agent task that can be delegated.
#[derive(Debug, Clone)]
pub struct AgentTask {
    /// Unique task identifier.
    pub id: String,
    /// The sub-agent type (e.g. "explore", "plan", "verify").
    pub agent_type: String,
    /// The task description / prompt.
    pub prompt: String,
    /// Model to use (None = use parent's model).
    pub model: Option<String>,
    /// Allowed tools for this sub-agent.
    pub allowed_tools: Vec<String>,
    /// Shared context for passing results between agents.
    pub context: AgentContext,
    /// Output from the sub-agent.
    pub output: Option<String>,
    /// Error message if the sub-agent failed.
    pub error: Option<String>,
}

impl AgentTask {
    /// Create a new agent task.
    #[must_use]
    pub fn new(id: &str, agent_type: &str, prompt: &str) -> Self {
        Self {
            id: id.to_string(),
            agent_type: agent_type.to_string(),
            prompt: prompt.to_string(),
            model: None,
            allowed_tools: Vec::new(),
            context: AgentContext::new(),
            output: None,
            error: None,
        }
    }

    /// Set the model for this task.
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
        self
    }

    /// Set allowed tools for this task.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Set the shared context for this task.
    #[must_use]
    pub fn with_context(mut self, context: AgentContext) -> Self {
        self.context = context;
        self
    }

    /// Check if the task completed successfully.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.output.is_some()
    }

    /// Check if the task failed.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.error.is_some()
    }
}

/// A registry that manages sub-agent tasks.
#[derive(Debug, Default)]
pub struct TaskRegistry {
    tasks: BTreeMap<String, AgentTask>,
}

impl TaskRegistry {
    /// Create a new empty task registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
        }
    }

    /// Register a task.
    pub fn register(&mut self, task: AgentTask) {
        self.tasks.insert(task.id.clone(), task);
    }

    /// Get a task by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&AgentTask> {
        self.tasks.get(id)
    }

    /// Get a mutable reference to a task.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut AgentTask> {
        self.tasks.get_mut(id)
    }

    /// Mark a task as completed with output.
    pub fn complete(&mut self, id: &str, output: &str) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| format!("task not found: {id}"))?;
        task.output = Some(output.to_string());
        Ok(())
    }

    /// Mark a task as failed with an error.
    pub fn fail(&mut self, id: &str, error: &str) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| format!("task not found: {id}"))?;
        task.error = Some(error.to_string());
        Ok(())
    }

    /// List all task IDs.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.tasks.keys().cloned().collect()
    }

    /// List all completed tasks.
    #[must_use]
    pub fn completed(&self) -> Vec<&AgentTask> {
        self.tasks.values().filter(|t| t.is_completed()).collect()
    }

    /// List all failed tasks.
    #[must_use]
    pub fn failed(&self) -> Vec<&AgentTask> {
        self.tasks.values().filter(|t| t.is_failed()).collect()
    }

    /// Get the number of registered tasks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Remove a task.
    pub fn remove(&mut self, id: &str) -> Option<AgentTask> {
        self.tasks.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_context_basic_operations() {
        let ctx = AgentContext::new();
        assert!(ctx.is_empty());

        ctx.set("key1", "value1");
        assert_eq!(ctx.get("key1"), Some("value1".to_string()));
        assert!(ctx.contains("key1"));
        assert_eq!(ctx.len(), 1);

        ctx.remove("key1");
        assert!(!ctx.contains("key1"));
        assert!(ctx.is_empty());
    }

    #[test]
    fn agent_context_is_shared_between_clones() {
        let ctx = AgentContext::new();
        let ctx2 = ctx.clone();

        ctx.set("shared", "data");
        assert_eq!(ctx2.get("shared"), Some("data".to_string()));
    }

    #[test]
    fn task_registry_manages_tasks() {
        let mut registry = TaskRegistry::new();

        let task = AgentTask::new("task-1", "explore", "Explore the codebase");
        registry.register(task);

        assert_eq!(registry.len(), 1);
        assert!(registry.get("task-1").is_some());

        registry
            .complete("task-1", "Found 5 files")
            .expect("complete");
        assert!(registry.get("task-1").unwrap().is_completed());

        assert_eq!(registry.completed().len(), 1);
        assert_eq!(registry.failed().len(), 0);
    }

    #[test]
    fn task_lifecycle() {
        let mut registry = TaskRegistry::new();
        let task = AgentTask::new("task-2", "verify", "Verify the changes")
            .with_model("sonnet")
            .with_tools(vec!["read_file".to_string(), "grep_search".to_string()]);
        registry.register(task);

        assert!(registry.get("task-2").unwrap().model.is_some());
        assert_eq!(registry.get("task-2").unwrap().allowed_tools.len(), 2);

        registry
            .fail("task-2", "Verification failed")
            .expect("fail");
        assert!(registry.get("task-2").unwrap().is_failed());
        assert_eq!(registry.failed().len(), 1);
    }

    #[test]
    fn task_not_found_errors() {
        let mut registry = TaskRegistry::new();

        let result = registry.complete("nonexistent", "output");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));

        let result = registry.fail("nonexistent", "error");
        assert!(result.is_err());
    }
}
