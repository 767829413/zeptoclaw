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
/// 2. The **inbound interceptor** on `MessageBus`, which calls [`resolve_by_id`]
///    (when the user clicked a specific approval card) or [`resolve`] (when the
///    user typed a plain "yes" / "no" with no request context, e.g. on Discord
///    or codex stdio).
///
/// Pending entries are stored as `(request_id, Sender)` pairs in FIFO order per
/// chat. [`resolve_by_id`] removes the matching entry by id; [`resolve`] pops
/// the oldest (the text-fallback path); [`resolve_all`] drains everything for
/// "yes all" / "no all".
///
/// Why both paths: AG-UI cards always carry a `requestId` and MUST resolve the
/// exact pending entry — without this the user could click "approve" on card B
/// and silently approve request A. Plain text channels (Discord raw "yes",
/// codex stdio prompt) have no id to thread through, so they fall back to FIFO.
pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, VecDeque<(String, oneshot::Sender<bool>)>>>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a pending approval for `(chat_id, request_id)`.
    ///
    /// Returns a receiver that completes when one of the resolve paths fires
    /// for this entry. The caller is responsible for generating a unique
    /// `request_id` (typically a ULID).
    pub fn register(&self, chat_id: &str, request_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("ApprovalBroker mutex poisoned")
            .entry(chat_id.to_string())
            .or_default()
            .push_back((request_id.to_string(), tx));
        rx
    }

    /// Resolve the pending approval whose `request_id` matches.
    ///
    /// Returns `true` if a matching entry was found and the receiver was still
    /// alive. Returns `false` if no entry matched (e.g. the request already
    /// timed out or the sandbox restarted) — callers should treat this as
    /// "not handled" and let the message continue downstream rather than
    /// silently resolving an unrelated request.
    pub fn resolve_by_id(&self, chat_id: &str, request_id: &str, approved: bool) -> bool {
        let mut guard = self
            .pending
            .lock()
            .expect("ApprovalBroker mutex poisoned");
        let Some(queue) = guard.get_mut(chat_id) else {
            return false;
        };
        let Some(idx) = queue.iter().position(|(id, _)| id == request_id) else {
            return false;
        };
        let (_, sender) = queue.remove(idx).expect("position was just found");
        let empty = queue.is_empty();
        if empty {
            guard.remove(chat_id);
        }
        sender.send(approved).is_ok()
    }

    /// Resolve the oldest pending approval for `chat_id`, regardless of id.
    ///
    /// Used by text-only channels (Discord raw "yes", codex stdio) where the
    /// user reply doesn't carry a request id. AG-UI / desktop callers MUST
    /// use [`resolve_by_id`] instead; using FIFO there leads to wrong-card
    /// approvals when multiple requests are pending.
    ///
    /// Returns `true` if a pending entry was found and resolved.
    pub fn resolve(&self, chat_id: &str, approved: bool) -> bool {
        let mut guard = self
            .pending
            .lock()
            .expect("ApprovalBroker mutex poisoned");
        let Some(queue) = guard.get_mut(chat_id) else {
            return false;
        };
        let Some((_, sender)) = queue.pop_front() else {
            return false;
        };
        if queue.is_empty() {
            guard.remove(chat_id);
        }
        sender.send(approved).is_ok()
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
        for (_, sender) in queue {
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
        let mut rx = broker.register("chat1", "req1");
        assert!(broker.has_pending("chat1"));
        assert!(broker.resolve("chat1", true));
        assert!(!broker.has_pending("chat1"));
        assert_eq!(rx.try_recv(), Ok(true));
    }

    #[test]
    fn register_and_resolve_denied() {
        let broker = ApprovalBroker::new();
        let mut rx = broker.register("chat1", "req1");
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
        let mut rx1 = broker.register("chat1", "req1");
        let mut rx2 = broker.register("chat1", "req2");
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
        let mut rx1 = broker.register("chat1", "req1");
        let mut rx2 = broker.register("chat1", "req2");
        let mut rx3 = broker.register("chat1", "req3");
        assert_eq!(broker.resolve_all("chat1", true), 3);
        assert_eq!(rx1.try_recv(), Ok(true));
        assert_eq!(rx2.try_recv(), Ok(true));
        assert_eq!(rx3.try_recv(), Ok(true));
        assert!(!broker.has_pending("chat1"));
    }

    // ---- resolve_by_id semantics ----

    #[test]
    fn resolve_by_id_targets_exact_request_not_fifo() {
        // Regression: previously broker was FIFO-only; an AG-UI card click
        // for the SECOND request would silently resolve the FIRST. Verify
        // resolve_by_id removes the entry by id, leaving the other intact.
        let broker = ApprovalBroker::new();
        let mut rx_a = broker.register("chat1", "req_a");
        let mut rx_b = broker.register("chat1", "req_b");
        assert_eq!(broker.pending_count("chat1"), 2);

        // User approves the second card explicitly.
        assert!(broker.resolve_by_id("chat1", "req_b", true));

        // req_b's receiver fired; req_a is untouched and still pending.
        assert_eq!(rx_b.try_recv(), Ok(true));
        assert_eq!(rx_a.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert_eq!(broker.pending_count("chat1"), 1);

        // Cleanup: resolving req_a still works after the out-of-order removal.
        assert!(broker.resolve_by_id("chat1", "req_a", false));
        assert_eq!(rx_a.try_recv(), Ok(false));
        assert!(!broker.has_pending("chat1"));
    }

    #[test]
    fn resolve_by_id_returns_false_for_unknown_id() {
        // Stale card click (sandbox already cleared the request, or wrong
        // chat) must NOT resolve any pending entry. The interceptor relies
        // on the false return to let the message fall through downstream
        // instead of silently swallowing it.
        let broker = ApprovalBroker::new();
        let mut rx = broker.register("chat1", "req_a");
        assert!(!broker.resolve_by_id("chat1", "req_does_not_exist", true));
        assert!(!broker.resolve_by_id("chat_other", "req_a", true));
        assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert_eq!(broker.pending_count("chat1"), 1);
    }

    #[test]
    fn resolve_by_id_cleans_up_chat_entry_when_emptied() {
        // After resolving the only pending request for a chat, the chat key
        // should not linger in the map (mirrors the cleanup `resolve` /
        // `resolve_all` perform). Otherwise long-lived sessions accumulate
        // empty queues.
        let broker = ApprovalBroker::new();
        let _rx = broker.register("chat1", "req_a");
        assert!(broker.has_pending("chat1"));
        assert!(broker.resolve_by_id("chat1", "req_a", true));
        assert!(!broker.has_pending("chat1"));
    }
}
