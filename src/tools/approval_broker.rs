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
/// Multiple pending approvals for the same chat are queued (FIFO). Structured
/// approval responses resolve by explicit `request_id`; legacy text replies can
/// still resolve the oldest pending request.
pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, VecDeque<PendingApproval>>>,
}

struct PendingApproval {
    request_id: String,
    sender: oneshot::Sender<bool>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a pending approval for `(chat_id, request_id)`.
    ///
    /// Returns a receiver that will complete when [`resolve`] (strict id
    /// match), [`resolve_oldest`] (legacy FIFO), or [`resolve_all`] is called.
    pub fn register(&self, chat_id: &str, request_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .entry(chat_id.to_string())
            .or_default()
            .push_back(PendingApproval {
                request_id: request_id.to_string(),
                sender: tx,
            });
        rx
    }

    /// Resolve one pending approval by explicit `request_id`.
    ///
    /// Returns `false` when the request does not exist.
    pub fn resolve(&self, chat_id: &str, request_id: &str, approved: bool) -> bool {
        let mut guard = self.pending.lock().expect("ApprovalBroker mutex poisoned");
        let Some(queue) = guard.get_mut(chat_id) else {
            return false;
        };
        let Some(idx) = queue.iter().position(|p| p.request_id == request_id) else {
            return false;
        };
        let Some(pending) = queue.remove(idx) else {
            return false;
        };
        let ok = pending.sender.send(approved).is_ok();
        if queue.is_empty() {
            guard.remove(chat_id);
        }
        ok
    }

    /// Resolve the oldest pending approval for `chat_id` (legacy FIFO path).
    pub fn resolve_oldest(&self, chat_id: &str, approved: bool) -> bool {
        let mut guard = self.pending.lock().expect("ApprovalBroker mutex poisoned");
        let Some(queue) = guard.get_mut(chat_id) else {
            return false;
        };
        let Some(pending) = queue.pop_front() else {
            return false;
        };
        let ok = pending.sender.send(approved).is_ok();
        if queue.is_empty() {
            guard.remove(chat_id);
        }
        ok
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
        for pending in queue {
            let _ = pending.sender.send(approved);
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

    /// Returns whether a specific pending request exists.
    pub fn has_pending_request(&self, chat_id: &str, request_id: &str) -> bool {
        self.pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .get(chat_id)
            .map(|q| q.iter().any(|p| p.request_id == request_id))
            .unwrap_or(false)
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
        let mut rx = broker.register("chat1", "r1");
        assert!(broker.has_pending("chat1"));
        assert!(broker.resolve("chat1", "r1", true));
        assert!(!broker.has_pending("chat1"));
        assert_eq!(rx.try_recv(), Ok(true));
    }

    #[test]
    fn register_and_resolve_denied() {
        let broker = ApprovalBroker::new();
        let mut rx = broker.register("chat1", "r1");
        assert!(broker.resolve("chat1", "r1", false));
        assert_eq!(rx.try_recv(), Ok(false));
    }

    #[test]
    fn resolve_without_pending_returns_false() {
        let broker = ApprovalBroker::new();
        assert!(!broker.resolve("nonexistent", "r1", true));
    }

    #[test]
    fn has_pending_returns_false_when_empty() {
        let broker = ApprovalBroker::new();
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn double_register_queues_both() {
        let broker = ApprovalBroker::new();
        let mut rx1 = broker.register("chat1", "r1");
        let mut rx2 = broker.register("chat1", "r2");
        assert_eq!(broker.pending_count("chat1"), 2);
        // Strict request-id match can resolve out of FIFO order.
        assert!(broker.resolve("chat1", "r2", true));
        assert_eq!(rx2.try_recv(), Ok(true));
        assert_eq!(broker.pending_count("chat1"), 1);
        // Remaining request still resolves correctly.
        assert!(broker.resolve("chat1", "r1", false));
        assert_eq!(rx1.try_recv(), Ok(false));
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn resolve_oldest_keeps_legacy_fifo() {
        let broker = ApprovalBroker::new();
        let mut rx1 = broker.register("chat1", "r1");
        let mut rx2 = broker.register("chat1", "r2");
        assert!(broker.resolve_oldest("chat1", true));
        assert_eq!(rx1.try_recv(), Ok(true));
        assert_eq!(broker.pending_count("chat1"), 1);
        assert!(broker.resolve_oldest("chat1", false));
        assert_eq!(rx2.try_recv(), Ok(false));
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn resolve_all_approves_every_pending() {
        let broker = ApprovalBroker::new();
        let mut rx1 = broker.register("chat1", "r1");
        let mut rx2 = broker.register("chat1", "r2");
        let mut rx3 = broker.register("chat1", "r3");
        assert_eq!(broker.resolve_all("chat1", true), 3);
        assert_eq!(rx1.try_recv(), Ok(true));
        assert_eq!(rx2.try_recv(), Ok(true));
        assert_eq!(rx3.try_recv(), Ok(true));
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn has_pending_request_checks_specific_id() {
        let broker = ApprovalBroker::new();
        let _rx = broker.register("chat1", "r1");
        assert!(broker.has_pending_request("chat1", "r1"));
        assert!(!broker.has_pending_request("chat1", "r2"));
    }

    #[test]
    fn resolve_with_wrong_request_id_returns_false() {
        let broker = ApprovalBroker::new();
        let _rx = broker.register("chat1", "r1");
        assert!(!broker.resolve("chat1", "r2", true));
        assert!(broker.has_pending_request("chat1", "r1"));
    }
}
