use std::path::{Path, PathBuf};

use runtime::Session;

/// Trait for loading resources such as session files and config.
pub trait ResourceLoader {
    /// Load a session from a path.
    fn load_session(&self, path: &Path) -> Result<Session, String>;
    /// Create a new session.
    fn create_session(&self) -> Session;
    /// Get the session directory, if any.
    fn session_dir(&self) -> Option<&Path>;
}

/// Default resource loader that reads from the filesystem.
#[derive(Debug, Clone)]
pub struct DefaultResourceLoader {
    session_dir: Option<PathBuf>,
}

impl DefaultResourceLoader {
    /// Create a new default resource loader.
    #[must_use]
    pub fn new(session_dir: Option<PathBuf>) -> Self {
        Self { session_dir }
    }

    /// Create a resource loader from an agent directory.
    #[must_use]
    pub fn from_agent_dir(agent_dir: &Path) -> Self {
        let session_dir = Some(agent_dir.join("sessions"));
        Self { session_dir }
    }
}

impl ResourceLoader for DefaultResourceLoader {
    fn load_session(&self, path: &Path) -> Result<Session, String> {
        Session::load_from_path(path).map_err(|e| e.to_string())
    }

    fn create_session(&self) -> Session {
        Session::new()
    }

    fn session_dir(&self) -> Option<&Path> {
        self.session_dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resource_loader_creates_and_loads_sessions() {
        let dir = std::env::temp_dir().join(format!("sdk-resource-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");

        let loader = DefaultResourceLoader::new(Some(dir.clone()));

        let session = loader.create_session();
        let path = dir.join("test.jsonl");
        session.save_to_path(&path).expect("save session");

        let loaded = loader.load_session(&path).expect("load session");
        assert_eq!(loaded.session_id, session.session_id);

        assert_eq!(loader.session_dir(), Some(dir.as_path()));

        fs::remove_dir_all(dir).expect("cleanup");
    }
}
