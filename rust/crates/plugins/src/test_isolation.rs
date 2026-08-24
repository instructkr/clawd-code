// Test isolation utilities for plugin tests
// ROADMAP #41: Stop ambient plugin state from skewing CLI regression checks

use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Lock for test environment isolation
pub struct EnvLock {
    _guard: std::sync::MutexGuard<'static, ()>,
    temp_home: PathBuf,
}

impl EnvLock {
    /// Acquire environment lock for test isolation
    pub fn lock() -> Self {
        let guard = ENV_LOCK.lock().unwrap();
        let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let temp_home = std::env::temp_dir().join(format!("plugin-test-{count}"));

        // Set up isolated environment
        std::fs::create_dir_all(&temp_home).ok();
        std::fs::create_dir_all(temp_home.join(".claude/plugins/installed")).ok();
        std::fs::create_dir_all(temp_home.join(".config")).ok();

        // Redirect HOME and XDG_CONFIG_HOME to temp directory
        env::set_var("HOME", &temp_home);
        env::set_var("XDG_CONFIG_HOME", temp_home.join(".config"));
        env::set_var("XDG_DATA_HOME", temp_home.join(".local/share"));

        EnvLock {
            _guard: guard,
            temp_home,
        }
    }

    /// Get the temporary home directory for this test
    #[must_use]
    pub fn temp_home(&self) -> &PathBuf {
        &self.temp_home
    }
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        // Cleanup temp directory
        std::fs::remove_dir_all(&self.temp_home).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_lock_creates_isolated_home() {
        let lock = EnvLock::lock();
        let home = env::var("HOME").unwrap();
        assert!(home.contains("plugin-test-"));
        assert_eq!(home, lock.temp_home().to_str().unwrap());
    }

    #[test]
    fn test_env_lock_creates_plugin_directories() {
        let lock = EnvLock::lock();
        let plugins_dir = lock.temp_home().join(".claude/plugins/installed");
        assert!(plugins_dir.exists());
    }
}
