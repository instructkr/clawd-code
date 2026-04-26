//! Notification dispatch subsystem for agent events.
//!
//! Provides a generic `NotificationSink` trait plus built-in sinks for
//! webhooks (via system `curl`), console output, and file logging.

use std::collections::HashSet;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Severity level for a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        };
        write!(f, "{label}")
    }
}

/// Category of event that triggered a notification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    TurnComplete,
    ToolError,
    ReviewRequired,
    ReviewApproved,
    ReviewRejected,
    SessionStarted,
    SessionEnded,
    ProviderError,
    Custom(String),
}

/// A notification to be dispatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Notification id.
    pub id: String,
    /// Event type.
    pub event: EventType,
    /// Human-readable message.
    pub message: String,
    /// Severity.
    pub severity: Severity,
    /// Timestamp in ms since epoch.
    pub timestamp_ms: u64,
    /// Optional tags for routing/filtering.
    pub tags: Vec<String>,
    /// Arbitrary payload data.
    pub payload: Option<serde_json::Value>,
}

impl Notification {
    /// Create a new notification with a generated id and current timestamp.
    #[must_use]
    pub fn new(event: EventType, message: impl Into<String>, severity: Severity) -> Self {
        Self {
            id: format!(
                "notif-{}",
                now_ms()
            ),
            event,
            message: message.into(),
            severity,
            timestamp_ms: now_ms(),
            tags: Vec::new(),
            payload: None,
        }
    }

    /// Add a tag.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add a payload.
    #[must_use]
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Serialize to pretty-printed JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| self.message.clone())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Sink trait
// ---------------------------------------------------------------------------

/// A destination for notifications.
pub trait NotificationSink: fmt::Debug + Send + Sync {
    /// Send the notification. Returns `Ok(())` if the sink accepted it.
    fn dispatch(&self, notification: &Notification) -> Result<(), String>;

    /// Sink name.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Built-in sinks
// ---------------------------------------------------------------------------

/// Log notifications to stderr.
#[derive(Debug, Clone)]
pub struct ConsoleSink;

impl NotificationSink for ConsoleSink {
    fn dispatch(&self, notification: &Notification) -> Result<(), String> {
        eprintln!(
            "[{}] {}  {}: {}",
            notification.severity,
            notification.name(),
            notification.event_label(),
            notification.message
        );
        Ok(())
    }

    fn name(&self) -> &str {
        "console"
    }
}

/// Append notifications as JSONL to a file.
#[derive(Debug, Clone)]
pub struct FileSink {
    pub path: std::path::PathBuf,
}

impl NotificationSink for FileSink {
    fn dispatch(&self, notification: &Notification) -> Result<(), String> {
        let line = serde_json::to_string(notification)
            .map_err(|e| format!("serialization failed: {e}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("failed to open {:?}: {e}", self.path))?;
        writeln!(file, "{line}").map_err(|e| format!("write failed: {e}"))?;
        Ok(())
    }

    fn name(&self) -> &str {
        "file"
    }
}

/// Dispatch to a Slack / Discord / generic webhook via system `curl`.
#[derive(Debug, Clone)]
pub struct WebhookSink {
    /// Full URL to POST to.
    pub url: String,
    /// Optional authorization header value.
    pub auth_header: Option<String>,
    /// HTTP timeout in seconds.
    pub timeout_secs: u64,
}

impl WebhookSink {
    /// Create a new webhook sink.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth_header: None,
            timeout_secs: 30,
        }
    }

    /// With authorization header.
    #[must_use]
    pub fn with_auth(mut self, auth: impl Into<String>) -> Self {
        self.auth_header = Some(auth.into());
        self
    }
}

impl NotificationSink for WebhookSink {
    fn dispatch(&self, notification: &Notification) -> Result<(), String> {
        let payload = notification.to_json();
        let mut cmd = Command::new("curl");
        cmd.arg("--silent")
            .arg("--show-error")
            .arg("--max-time")
            .arg(self.timeout_secs.to_string())
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(&payload)
            .arg(&self.url);

        if let Some(auth) = &self.auth_header {
            cmd.arg("-H").arg(format!("Authorization: {auth}"));
        }

        let output = cmd.output().map_err(|e| format!("curl failed: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("webhook returned {}: {stderr}", output.status));
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "webhook"
    }
}

/// Email sink (stub: captures SMTP config; actual delivery in CLI).
#[derive(Debug, Clone)]
pub struct EmailSink {
    /// SMTP server.
    pub smtp_host: String,
    /// SMTP port.
    pub smtp_port: u16,
    /// From address.
    pub from: String,
    /// To addresses.
    pub to: Vec<String>,
    /// Username for SMTP auth.
    pub username: Option<String>,
    /// Password for SMTP auth (store securely in production).
    pub password: Option<String>,
}

impl NotificationSink for EmailSink {
    fn dispatch(&self, notification: &Notification) -> Result<(), String> {
        let subject = format!("[{}] {}", notification.severity, notification.event_label());
        let body = notification.to_json();

        let mut cmd = Command::new("curl");
        cmd.arg("--silent")
            .arg("--show-error")
            .arg("--max-time")
            .arg("30")
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(format!(
                "{{\"to\": {:?}, \"subject\": \"{subject}\", \"body\": \"{body}\"}}",
                self.to
            ));

        // Stub: in production, use an SMTP crate or external mail service API
        eprintln!(
            "[email] Would send to {:?} via {}:{}",
            self.to, self.smtp_host, self.smtp_port
        );
        Ok(())
    }

    fn name(&self) -> &str {
        "email"
    }
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

/// A rule that decides whether a notification should be passed to a sink.
#[derive(Debug, Clone, Default)]
pub struct NotificationFilter {
    /// Minimum severity to pass.
    pub min_severity: Option<Severity>,
    /// Allowed event types (empty = all).
    pub event_types: HashSet<EventType>,
    /// Required tags (all must be present).
    pub required_tags: HashSet<String>,
    /// Excluded event types.
    pub excluded_events: HashSet<EventType>,
}

impl NotificationFilter {
    /// Allow events of a specific type.
    #[must_use]
    pub fn allow_event(mut self, event: EventType) -> Self {
        self.event_types.insert(event);
        self
    }

    /// Exclude events of a specific type.
    #[must_use]
    pub fn exclude_event(mut self, event: EventType) -> Self {
        self.excluded_events.insert(event);
        self
    }

    /// Set minimum severity.
    #[must_use]
    pub fn with_min_severity(mut self, severity: Severity) -> Self {
        self.min_severity = Some(severity);
        self
    }

    /// Require a tag.
    #[must_use]
    pub fn require_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tags.insert(tag.into());
        self
    }

    /// Check whether a notification matches this filter.
    #[must_use]
    pub fn matches(&self, notification: &Notification) -> bool {
        if self.excluded_events.contains(&notification.event) {
            return false;
        }
        if let Some(min) = &self.min_severity {
            if notification.severity < *min {
                return false;
            }
        }
        if !self.event_types.is_empty() && !self.event_types.contains(&notification.event) {
            return false;
        }
        if !self.required_tags.is_empty() {
            let tags: HashSet<String> = notification.tags.clone().into_iter().collect();
            if !self.required_tags.is_subset(&tags) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// A registered sink with its filter.
#[derive(Debug)]
pub struct SinkRegistration {
    /// The sink to dispatch to.
    pub sink: Box<dyn NotificationSink>,
    /// Filter for this sink.
    pub filter: NotificationFilter,
}

/// Routes notifications to matching sinks.
#[derive(Debug, Default)]
pub struct NotificationDispatcher {
    sinks: Vec<SinkRegistration>,
}

impl NotificationDispatcher {
    /// Create a new dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    /// Register a sink with a filter.
    pub fn register(
        &mut self, sink: Box<dyn NotificationSink>, filter: NotificationFilter) {
        self.sinks.push(SinkRegistration { sink, filter });
    }

    /// Dispatch a notification to all matching sinks.
    /// Returns a vector of (sink_name, result) for diagnostics.
    pub fn dispatch(
        &self, notification: &Notification) -> Vec<(&str, Result<(), String>)> {
        self.sinks
            .iter()
            .filter(|reg| reg.filter.matches(notification))
            .map(|reg| (reg.sink.name(), reg.sink.dispatch(notification)))
            .collect()
    }

    /// Number of registered sinks.
    #[must_use]
    pub fn sink_count(&self) -> usize {
        self.sinks.len()
    }
}

// ---------------------------------------------------------------------------
// Extension trait helpers
// ---------------------------------------------------------------------------

trait NotificationExt {
    fn name(&self) -> &str;
    fn event_label(&self) -> String;
}

impl NotificationExt for Notification {
    fn name(&self) -> &str {
        &self.id
    }

    fn event_label(&self) -> String {
        match &self.event {
            EventType::Custom(s) => s.clone(),
            _ => format!("{:?}", self.event).to_lowercase(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct CountingSink {
        name: String,
        count: Arc<AtomicUsize>,
    }

    impl NotificationSink for CountingSink {
        fn dispatch(&self, _n: &Notification) -> Result<(), String> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn test_notification(event: EventType, severity: Severity) -> Notification {
        Notification::new(event, "test message", severity)
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Debug < Severity::Info);
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Critical);
    }

    #[test]
    fn notification_builder_with_tag() {
        let n = Notification::new(EventType::TurnComplete, "msg", Severity::Info)
            .with_tag("agent-1");
        assert_eq!(n.tags, vec!["agent-1"]);
    }

    #[test]
    fn filter_min_severity_blocks_lower() {
        let filter = NotificationFilter::default().with_min_severity(Severity::Warning);
        let info = test_notification(EventType::TurnComplete, Severity::Info);
        let warn = test_notification(EventType::ToolError, Severity::Warning);
        assert!(!filter.matches(&info));
        assert!(filter.matches(&warn));
    }

    #[test]
    fn filter_event_type_only_allows_matching() {
        let filter = NotificationFilter::default().allow_event(EventType::SessionStarted);
        let start = test_notification(EventType::SessionStarted, Severity::Info);
        let end = test_notification(EventType::SessionEnded, Severity::Info);
        assert!(filter.matches(&start));
        assert!(!filter.matches(&end));
    }

    #[test]
    fn filter_required_tags() {
        let filter = NotificationFilter::default().require_tag("urgent");
        let tagged = Notification::new(EventType::ReviewRequired, "msg", Severity::Info)
            .with_tag("urgent");
        let plain = test_notification(EventType::ReviewRequired, Severity::Info);
        assert!(filter.matches(&tagged));
        assert!(!filter.matches(&plain));
    }

    #[test]
    fn filter_exclude_event() {
        let filter = NotificationFilter::default().exclude_event(EventType::TurnComplete);
        let n = test_notification(EventType::TurnComplete, Severity::Info);
        assert!(!filter.matches(&n));
        let other = test_notification(EventType::SessionStarted, Severity::Info);
        assert!(filter.matches(&other));
    }

    #[test]
    fn console_sink_dispatches_without_error() {
        let sink = ConsoleSink;
        let n = test_notification(EventType::TurnComplete, Severity::Info);
        assert!(sink.dispatch(&n).is_ok());
    }

    #[test]
    fn counting_sink_tracks_dispatches() {
        let count = Arc::new(AtomicUsize::new(0));
        let sink = CountingSink {
            name: "counter".to_string(),
            count: Arc::clone(&count),
        };
        let n = test_notification(EventType::TurnComplete, Severity::Info);
        sink.dispatch(&n).expect("dispatch");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatcher_routes_to_matching_sinks() {
        let count = Arc::new(AtomicUsize::new(0));
        let sink = Box::new(CountingSink {
            name: "counter".to_string(),
            count: Arc::clone(&count),
        });

        let mut dispatcher = NotificationDispatcher::new();
        dispatcher.register(
            sink,
            NotificationFilter::default().allow_event(EventType::TurnComplete),
        );

        let matched = dispatcher.dispatch(&test_notification(EventType::TurnComplete, Severity::Info));
        assert_eq!(matched.len(), 1);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let unmatched = dispatcher.dispatch(&test_notification(EventType::SessionStarted, Severity::Info));
        assert_eq!(unmatched.len(), 0);
    }

    #[test]
    fn file_sink_writes_jsonl() {
        let tmp = std::env::temp_dir().join(format!("sdk-notif-{}.jsonl", now_ms()));
        let sink = FileSink { path: tmp.clone() };
        let n = test_notification(EventType::ReviewRequired, Severity::Warning);
        sink.dispatch(&n).expect("dispatch");

        let content = std::fs::read_to_string(&tmp).expect("read");
        assert!(content.contains("review_required"));
        assert!(content.contains("test message"));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn webhook_sink_serialization() {
        let sink = WebhookSink::new("https://hooks.example.com/webhook").with_auth("Bearer xyz");
        assert_eq!(sink.url, "https://hooks.example.com/webhook");
        assert_eq!(sink.auth_header, Some("Bearer xyz".to_string()));
    }

    #[test]
    fn email_sink_stub_runs() {
        let sink = EmailSink {
            smtp_host: "smtp.test".to_string(),
            smtp_port: 587,
            from: "bot@test".to_string(),
            to: vec!["user@test".to_string()],
            username: None,
            password: None,
        };
        let n = test_notification(EventType::ProviderError, Severity::Error);
        assert!(sink.dispatch(&n).is_ok());
    }

    #[test]
    fn notification_serde_round_trip() {
        let n = Notification::new(EventType::ReviewApproved, "lgtm", Severity::Info)
            .with_tag("team-a");
        let json = serde_json::to_string(&n).expect("serialize");
        let parsed: Notification = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, n.id);
        assert_eq!(parsed.message, n.message);
        assert_eq!(parsed.severity, n.severity);
        assert_eq!(parsed.tags, n.tags);
    }

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Critical.to_string(), "critical");
    }
}
