use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

use api::{build_http_client_with, ProxyConfig};

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let original = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
#[cfg(not(windows))]
fn proxy_config_from_env_reads_uppercase_proxy_vars() {
    // given
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", Some("http://proxy.corp:3128"));
    let _https = EnvVarGuard::set("HTTPS_PROXY", Some("http://secure.corp:3129"));
    let _no = EnvVarGuard::set("NO_PROXY", Some("localhost,127.0.0.1"));
    let _http_lower = EnvVarGuard::set("http_proxy", None);
    let _https_lower = EnvVarGuard::set("https_proxy", None);
    let _no_lower = EnvVarGuard::set("no_proxy", None);

    // when
    let config = ProxyConfig::from_env();

    // then
    assert_eq!(config.http_proxy.as_deref(), Some("http://proxy.corp:3128"));
    assert_eq!(
        config.https_proxy.as_deref(),
        Some("http://secure.corp:3129")
    );
    assert_eq!(config.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
    assert!(config.proxy_url.is_none());
    assert!(!config.is_empty());
}

#[test]
#[cfg(windows)]
fn proxy_config_from_env_reads_proxy_vars_on_windows() {
    // Windows treats environment variables as case-insensitive, so we only
    // assert that setting the proxy variables is reflected in the config.
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", Some("http://proxy.corp:3128"));
    let _https = EnvVarGuard::set("HTTPS_PROXY", Some("http://secure.corp:3129"));
    let _no = EnvVarGuard::set("NO_PROXY", Some("localhost,127.0.0.1"));

    let config = ProxyConfig::from_env();

    assert_eq!(config.http_proxy.as_deref(), Some("http://proxy.corp:3128"));
    assert_eq!(
        config.https_proxy.as_deref(),
        Some("http://secure.corp:3129")
    );
    assert_eq!(config.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
    assert!(config.proxy_url.is_none());
    assert!(!config.is_empty());
}

#[test]
fn proxy_config_from_env_reads_lowercase_proxy_vars() {
    // given
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", None);
    let _https = EnvVarGuard::set("HTTPS_PROXY", None);
    let _no = EnvVarGuard::set("NO_PROXY", None);
    let _http_lower = EnvVarGuard::set("http_proxy", Some("http://lower.corp:3128"));
    let _https_lower = EnvVarGuard::set("https_proxy", Some("http://lower-secure.corp:3129"));
    let _no_lower = EnvVarGuard::set("no_proxy", Some(".internal"));

    // when
    let config = ProxyConfig::from_env();

    // then
    assert_eq!(config.http_proxy.as_deref(), Some("http://lower.corp:3128"));
    assert_eq!(
        config.https_proxy.as_deref(),
        Some("http://lower-secure.corp:3129")
    );
    assert_eq!(config.no_proxy.as_deref(), Some(".internal"));
    assert!(!config.is_empty());
}

#[test]
fn proxy_config_from_env_is_empty_when_no_vars_set() {
    // given
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", None);
    let _https = EnvVarGuard::set("HTTPS_PROXY", None);
    let _no = EnvVarGuard::set("NO_PROXY", None);
    let _http_lower = EnvVarGuard::set("http_proxy", None);
    let _https_lower = EnvVarGuard::set("https_proxy", None);
    let _no_lower = EnvVarGuard::set("no_proxy", None);

    // when
    let config = ProxyConfig::from_env();

    // then
    assert!(config.is_empty());
    assert!(config.http_proxy.is_none());
    assert!(config.https_proxy.is_none());
    assert!(config.no_proxy.is_none());
}

#[test]
fn proxy_config_from_env_treats_empty_values_as_unset() {
    // given
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", Some(""));
    let _https = EnvVarGuard::set("HTTPS_PROXY", Some(""));
    let _http_lower = EnvVarGuard::set("http_proxy", Some(""));
    let _https_lower = EnvVarGuard::set("https_proxy", Some(""));
    let _no = EnvVarGuard::set("NO_PROXY", Some(""));
    let _no_lower = EnvVarGuard::set("no_proxy", Some(""));

    // when
    let config = ProxyConfig::from_env();

    // then
    assert!(config.is_empty());
}

#[test]
fn build_client_with_env_proxy_config_succeeds() {
    // given
    let _lock = env_lock();
    let _http = EnvVarGuard::set("HTTP_PROXY", Some("http://proxy.corp:3128"));
    let _https = EnvVarGuard::set("HTTPS_PROXY", Some("http://secure.corp:3129"));
    let _no = EnvVarGuard::set("NO_PROXY", Some("localhost"));
    let _http_lower = EnvVarGuard::set("http_proxy", None);
    let _https_lower = EnvVarGuard::set("https_proxy", None);
    let _no_lower = EnvVarGuard::set("no_proxy", None);
    let config = ProxyConfig::from_env();

    // when
    let result = build_http_client_with(&config);

    // then
    assert!(result.is_ok());
}

#[test]
fn build_client_with_proxy_url_config_succeeds() {
    // given
    let config = ProxyConfig::from_proxy_url("http://unified.corp:3128");

    // when
    let result = build_http_client_with(&config);

    // then
    assert!(result.is_ok());
}

#[test]
#[cfg(not(windows))]
fn proxy_config_from_env_prefers_uppercase_over_lowercase() {
    // given
    let _lock = env_lock();
    let _http_upper = EnvVarGuard::set("HTTP_PROXY", Some("http://upper.corp:3128"));
    let _http_lower = EnvVarGuard::set("http_proxy", Some("http://lower.corp:3128"));
    let _https = EnvVarGuard::set("HTTPS_PROXY", None);
    let _https_lower = EnvVarGuard::set("https_proxy", None);
    let _no = EnvVarGuard::set("NO_PROXY", None);
    let _no_lower = EnvVarGuard::set("no_proxy", None);

    // when
    let config = ProxyConfig::from_env();

    // then
    assert_eq!(config.http_proxy.as_deref(), Some("http://upper.corp:3128"));
}
