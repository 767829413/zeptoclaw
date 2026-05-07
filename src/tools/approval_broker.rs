//! Shared approval broker for coordinating tool approval across async boundaries.
//!
//! The `ApprovalBroker` manages pending approval requests using oneshot channels,
//! allowing the approval handler (agent loop) to wait for a user response while
//! the inbound message interceptor (on the `MessageBus`) resolves it.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Coordinates pending tool-approval requests.
///
/// A single `ApprovalBroker` is shared (via `Arc`) between:
/// 1. The **approval handler** registered on the agent, which calls [`register`]
///    and then awaits the returned receiver.
/// 2. The **inbound interceptor** on `MessageBus`, which calls [`resolve`]
///    when a user replies "yes" / "no".
///
/// Multiple pending approvals for the same chat are queued (FIFO). Each "yes"
/// resolves the oldest pending request; "yes all" / "no all" resolves every
/// pending request at once.
pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, VecDeque<oneshot::Sender<bool>>>>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a pending approval for `chat_id`.
    ///
    /// Returns a receiver that will complete when [`resolve`] is called for
    /// the same `chat_id`. Multiple pending requests are queued; each call
    /// to [`resolve`] services the oldest one.
    pub fn register(&self, chat_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .entry(chat_id.to_string())
            .or_default()
            .push_back(tx);
        rx
    }

    /// Resolve the oldest pending approval for `chat_id`.
    ///
    /// Returns `true` if there was a pending request that was successfully
    /// resolved. Returns `false` if no pending request existed or the
    /// receiver was already dropped.
    pub fn resolve(&self, chat_id: &str, approved: bool) -> bool {
        let tx = self
            .pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .get_mut(chat_id)
            .and_then(|q| q.pop_front());
        match tx {
            Some(sender) => sender.send(approved).is_ok(),
            None => false,
        }
    }

    /// Resolve ALL pending approvals for `chat_id` with the same decision.
    ///
    /// Returns the number of requests resolved. Used for "yes all" / "no all".
    pub fn resolve_all(&self, chat_id: &str, approved: bool) -> usize {
        let queue = self
            .pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .remove(chat_id)
            .unwrap_or_default();
        let count = queue.len();
        for sender in queue {
            let _ = sender.send(approved);
        }
        count
    }

    /// Return the number of pending approvals for `chat_id`.
    pub fn pending_count(&self, chat_id: &str) -> usize {
        self.pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .get(chat_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Check whether any pending approval exists for `chat_id`.
    pub fn has_pending(&self, chat_id: &str) -> bool {
        self.pending_count(chat_id) > 0
    }
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_resolve_approved() {
        let broker = ApprovalBroker::new();
        let mut rx = broker.register("chat1");
        assert!(broker.has_pending("chat1"));
        assert!(broker.resolve("chat1", true));
        assert!(!broker.has_pending("chat1"));
        assert_eq!(rx.try_recv(), Ok(true));
    }

    #[test]
    fn register_and_resolve_denied() {
        let broker = ApprovalBroker::new();
        let mut rx = broker.register("chat1");
        assert!(broker.resolve("chat1", false));
        assert_eq!(rx.try_recv(), Ok(false));
    }

    #[test]
    fn resolve_without_pending_returns_false() {
        let broker = ApprovalBroker::new();
        assert!(!broker.resolve("nonexistent", true));
    }

    #[test]
    fn has_pending_returns_false_when_empty() {
        let broker = ApprovalBroker::new();
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn double_register_queues_both() {
        let broker = ApprovalBroker::new();
        let mut rx1 = broker.register("chat1");
        let mut rx2 = broker.register("chat1");
        assert_eq!(broker.pending_count("chat1"), 2);
        // First resolve services the oldest (rx1)
        assert!(broker.resolve("chat1", true));
        assert_eq!(rx1.try_recv(), Ok(true));
        assert_eq!(broker.pending_count("chat1"), 1);
        // Second resolve services rx2
        assert!(broker.resolve("chat1", false));
        assert_eq!(rx2.try_recv(), Ok(false));
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn resolve_all_approves_every_pending() {
        let broker = ApprovalBroker::new();
        let mut rx1 = broker.register("chat1");
        let mut rx2 = broker.register("chat1");
        let mut rx3 = broker.register("chat1");
        assert_eq!(broker.resolve_all("chat1", true), 3);
        assert_eq!(rx1.try_recv(), Ok(true));
        assert_eq!(rx2.try_recv(), Ok(true));
        assert_eq!(rx3.try_recv(), Ok(true));
        assert!(!broker.has_pending("chat1"));
    }
}
