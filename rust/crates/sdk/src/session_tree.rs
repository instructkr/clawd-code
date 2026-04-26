//! Session tree management with id/parentId branching support.
//!
//! Sessions are stored as tree structures where each entry carries an `id`
//! and optional `parent_id`, enabling in-place branching without creating
//! new files.

/// A node in the session tree, representing one message exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTreeNode {
    /// Unique identifier for this node.
    pub id: String,
    /// ID of the parent node (None for root).
    pub parent_id: Option<String>,
    /// The role of this entry ("user", "assistant", "`tool_result`").
    pub role: String,
    /// A summary or label for this node.
    pub label: Option<String>,
    /// IDs of child nodes (single source of truth lives in the SessionTree's BTreeMap).
    pub children: Vec<String>,
}

/// A tree-structured session that supports branching.
///
/// All nodes live in a single flat `BTreeMap` keyed by ID. Parent-child
/// relationships are maintained through `parent_id` back-links on each node
/// and `children` ID lists. This avoids data duplication and keeps mutations
/// consistent — there is only one copy of each node.
#[derive(Debug, Clone)]
pub struct SessionTree {
    /// All nodes in the tree, indexed by ID (single source of truth).
    nodes: std::collections::BTreeMap<String, SessionTreeNode>,
    /// The root node ID.
    root_id: Option<String>,
    /// The current active leaf node ID.
    active_id: Option<String>,
}

impl SessionTree {
    /// Create a new empty session tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: std::collections::BTreeMap::new(),
            root_id: None,
            active_id: None,
        }
    }

    /// Add a root node to the tree.
    pub fn set_root(&mut self, id: &str, role: &str, label: Option<String>) {
        let node = SessionTreeNode {
            id: id.to_string(),
            parent_id: None,
            role: role.to_string(),
            label,
            children: Vec::new(),
        };
        self.nodes.insert(id.to_string(), node);
        self.root_id = Some(id.to_string());
        self.active_id = Some(id.to_string());
    }

    /// Add a child node under the given parent.
    pub fn add_child(
        &mut self,
        id: &str,
        parent_id: &str,
        role: &str,
        label: Option<String>,
    ) -> Result<(), String> {
        if !self.nodes.contains_key(parent_id) {
            return Err(format!("parent node not found: {parent_id}"));
        }
        if self.nodes.contains_key(id) {
            return Err(format!("node already exists: {id}"));
        }
        let node = SessionTreeNode {
            id: id.to_string(),
            parent_id: Some(parent_id.to_string()),
            role: role.to_string(),
            label,
            children: Vec::new(),
        };
        // Record child ID on the parent (no data duplication)
        if let Some(parent) = self.nodes.get_mut(parent_id) {
            parent.children.push(id.to_string());
        }
        self.nodes.insert(id.to_string(), node);
        self.active_id = Some(id.to_string());
        Ok(())
    }

    /// Get a node by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SessionTreeNode> {
        self.nodes.get(id)
    }

    /// Get the root node.
    #[must_use]
    pub fn root(&self) -> Option<&SessionTreeNode> {
        self.root_id.as_ref().and_then(|id| self.nodes.get(id))
    }

    /// Get the active leaf node.
    #[must_use]
    pub fn active(&self) -> Option<&SessionTreeNode> {
        self.active_id.as_ref().and_then(|id| self.nodes.get(id))
    }

    /// Get the path from root to the active node.
    #[must_use]
    pub fn active_path(&self) -> Vec<&SessionTreeNode> {
        let mut path = Vec::new();
        let mut current = self.active_id.as_ref().and_then(|id| self.nodes.get(id));
        while let Some(node) = current {
            path.push(node);
            current = node.parent_id.as_ref().and_then(|pid| self.nodes.get(pid));
        }
        path.reverse();
        path
    }

    /// Fork the tree at the given node, creating a new branch.
    /// The new node is a sibling of the forked node (same parent).
    pub fn fork_at(&mut self, node_id: &str, new_branch_id: &str) -> Result<(), String> {
        if !self.nodes.contains_key(node_id) {
            return Err(format!("node not found: {node_id}"));
        }
        let node = self
            .nodes
            .get(node_id)
            .ok_or_else(|| format!("node not found: {node_id}"))?;
        let parent_id = node.parent_id.clone();
        let role = node.role.clone();
        let label = node.label.clone();

        // Fork at root: create a new root sibling
        match &parent_id {
            Some(pid) => self.add_child(new_branch_id, pid, &role, label)?,
            None => {
                // Forking the root creates a second root-like node.
                // We preserve the original root_id so the tree remains
                // reachable. The new node gets its own entry.
                let new_node = SessionTreeNode {
                    id: new_branch_id.to_string(),
                    parent_id: None,
                    role: role.clone(),
                    label,
                    children: Vec::new(),
                };
                self.nodes.insert(new_branch_id.to_string(), new_node);
                self.active_id = Some(new_branch_id.to_string());
            }
        }
        Ok(())
    }

    /// Navigate to a specific node (make it the active node).
    pub fn navigate_to(&mut self, node_id: &str) -> Result<(), String> {
        if !self.nodes.contains_key(node_id) {
            return Err(format!("node not found: {node_id}"));
        }
        self.active_id = Some(node_id.to_string());
        Ok(())
    }

    /// Get the active node ID.
    #[must_use]
    pub fn active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }
}

impl Default for SessionTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_single_node_tree() {
        let mut tree = SessionTree::new();
        tree.set_root("root-1", "user", Some("Initial prompt".to_string()));
        assert!(tree.root().is_some());
        assert_eq!(tree.active().unwrap().id, "root-1");
    }

    #[test]
    fn adds_child_nodes() {
        let mut tree = SessionTree::new();
        tree.set_root("r1", "user", None);
        tree.add_child("c1", "r1", "assistant", None)
            .expect("add child");
        tree.add_child("c2", "c1", "user", Some("Follow up".to_string()))
            .expect("add second child");

        let path = tree.active_path();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id, "r1");
        assert_eq!(path[1].id, "c1");
        assert_eq!(path[2].id, "c2");

        // Verify parent-child relationships are via IDs, not duplicated data
        assert_eq!(tree.get("r1").unwrap().children, vec!["c1".to_string()]);
        assert_eq!(tree.get("c1").unwrap().children, vec!["c2".to_string()]);
    }

    #[test]
    fn forks_at_node() {
        let mut tree = SessionTree::new();
        tree.set_root("r1", "user", None);
        tree.add_child("c1", "r1", "assistant", None)
            .expect("add child");

        // Fork at root — creates a new root-level node
        tree.fork_at("r1", "forked-c2").expect("fork");
        assert!(tree.get("forked-c2").is_some());
        assert_eq!(tree.active().unwrap().id, "forked-c2");
        // Original root is still reachable
        assert_eq!(tree.root().unwrap().id, "r1");
    }

    #[test]
    fn forks_at_non_root() {
        let mut tree = SessionTree::new();
        tree.set_root("r1", "user", None);
        tree.add_child("c1", "r1", "assistant", None)
            .expect("add child");

        // Fork at c1 — creates a sibling under r1
        tree.fork_at("c1", "forked-c1b").expect("fork at non-root");
        assert!(tree.get("forked-c1b").is_some());
        assert_eq!(tree.get("forked-c1b").unwrap().parent_id, Some("r1".to_string()));
        // Both c1 and forked-c1b are children of r1
        let r1_children = &tree.get("r1").unwrap().children;
        assert!(r1_children.contains(&"c1".to_string()));
        assert!(r1_children.contains(&"forked-c1b".to_string()));
    }

    #[test]
    fn navigates_to_node() {
        let mut tree = SessionTree::new();
        tree.set_root("r1", "user", None);
        tree.add_child("c1", "r1", "assistant", None)
            .expect("add child");

        tree.navigate_to("r1").expect("navigate to root");
        assert_eq!(tree.active().unwrap().id, "r1");
    }

    #[test]
    fn unknown_node_returns_none() {
        let tree = SessionTree::new();
        assert!(tree.get("nonexistent").is_none());
        assert!(tree.root().is_none());
    }

    #[test]
    fn adding_child_to_missing_parent_fails() {
        let mut tree = SessionTree::new();
        let result = tree.add_child("c1", "nonexistent", "user", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }
}
