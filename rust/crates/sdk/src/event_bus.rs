use std::sync::mpsc::{self, Receiver, Sender};

use runtime::{AssistantEvent, AutoCompactionEvent, TurnSummary};

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Events emitted during an agent session lifecycle.
#[derive(Debug, Clone)]
pub enum AgentSessionEvent {
    /// A turn has started.
    TurnStarted,
    /// A turn has completed with its summary.
    TurnCompleted(TurnSummary),
    /// A text delta was received from the provider stream.
    TextDelta(String),
    /// A tool use was requested by the model.
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    /// A tool execution started.
    ToolExecutionStarted { name: String },
    /// A tool execution completed with its result.
    ToolExecutionCompleted {
        name: String,
        result: String,
        is_error: bool,
    },
    /// An auto-compaction event occurred.
    AutoCompaction(AutoCompactionEvent),
    /// An assistant event was received from the API.
    AssistantEvent(AssistantEvent),
    /// A session lifecycle event occurred.
    SessionLifecycle(SessionLifecycleEvent),
    /// A turn lifecycle event occurred.
    TurnEvent(TurnEvent),
    /// A tool execution event occurred.
    ToolExecution(ToolExecutionEvent),
    /// An error occurred.
    Error(String),
}

/// Session lifecycle events.
#[derive(Debug, Clone)]
pub enum SessionLifecycleEvent {
    Created { session_id: String },
    Loaded { session_id: String },
    Saved { session_id: String },
    Closed { session_id: String },
    ModelChanged { previous: String, current: String },
    CompactionStarted,
    CompactionCompleted { removed_count: usize },
}

/// Turn lifecycle events.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    Started,
    Completed(TurnSummary),
}

/// Tool execution events.
#[derive(Debug, Clone)]
pub enum ToolExecutionEvent {
    Started {
        name: String,
        tool_call_id: String,
    },
    Completed {
        name: String,
        tool_call_id: String,
        output: String,
        is_error: bool,
    },
}

/// A handle returned from subscribing to events. Dropping it unsubscribes.
#[must_use = "dropping the subscription handle unsubscribes the receiver"]
pub struct EventSubscription {
    _receiver: Receiver<AgentSessionEvent>,
}

impl EventSubscription {
    /// Try to receive the next event without blocking.
    #[must_use]
    pub fn try_recv(&self) -> Option<AgentSessionEvent> {
        self._receiver.try_recv().ok()
    }

    /// Block and wait for the next event.
    pub fn recv(&self) -> Result<AgentSessionEvent, mpsc::RecvError> {
        self._receiver.recv()
    }
}

/// A simple multi-producer, multi-consumer event bus.
///
/// Subscribers receive all events emitted during session lifecycle.
#[derive(Debug, Clone)]
pub struct EventBus {
    subscribers: Vec<Sender<AgentSessionEvent>>,
}

impl EventBus {
    /// Create a new event bus with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// Subscribe to all events. Returns a handle that unsubscribes on drop.
    pub fn subscribe(&mut self) -> EventSubscription {
        let (tx, rx) = mpsc::channel();
        self.subscribers.push(tx);
        EventSubscription { _receiver: rx }
    }

    /// Emit an event to all subscribers. Silently drops dead subscribers.
    pub fn emit(&mut self, event: AgentSessionEvent) {
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Remove all subscribers that have been dropped.
    pub fn prune_dead(&mut self) {
        self.subscribers
            .retain(|tx| tx.send(AgentSessionEvent::TurnStarted).is_ok());
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bus_broadcasts_to_subscribers() {
        let mut bus = EventBus::new();
        let sub1 = bus.subscribe();
        let sub2 = bus.subscribe();

        bus.emit(AgentSessionEvent::TurnStarted);

        assert!(matches!(sub1.recv(), Ok(AgentSessionEvent::TurnStarted)));
        assert!(matches!(sub2.recv(), Ok(AgentSessionEvent::TurnStarted)));
    }

    #[test]
    fn event_bus_removes_dead_subscribers() {
        let mut bus = EventBus::new();
        let sub = bus.subscribe();
        drop(sub); // Drop the subscriber

        // This should not panic and should clean up the dead subscriber
        bus.emit(AgentSessionEvent::TurnStarted);
        assert!(bus.subscribers.is_empty());
    }

    #[test]
    fn event_subscription_try_recv_returns_none_when_empty() {
        let mut bus = EventBus::new();
        let sub = bus.subscribe();
        assert!(sub.try_recv().is_none());
    }
}
