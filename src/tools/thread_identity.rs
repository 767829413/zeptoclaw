//! Process-level `(user, agent)` identity for HITL approval routing.
//!
//! The zeptoclaw sandbox is a per-`(user, agent)` container in M7. The
//! `ApprovalBroker` keys its pending entries on a stable [`ThreadKey`] so
//! that ACP session churn (e.g. SSE rebuilds) doesn't desync the user's
//! "yes" / "no" reply from an in-flight approval.
//!
//! This module owns the singleton that tells the rest of the process
//! which `(user, agent)` it is serving. It is intentionally minimal:
//!
//! - `ThreadIdentity::Known(ThreadKey)`: the typical container case;
//!   filled from `thread_meta.json` written by the scheduler. **PR1 does
//!   not yet write the file** (that's PR2 — see
//!   `docs/plans/doing/zeptoclaw-thread-approval.md` §9 changelog); the
//!   loader is wired now so PR1's plumbing is complete and PR2 only adds
//!   the writer.
//! - `ThreadIdentity::Unknown`: dev / in-process callers without a meta
//!   file. The broker falls back to `ThreadKey::from_chat_id_fallback` so
//!   behaviour matches the pre-PR1 baseline.
//!
//! The loader is best-effort and never panics: a missing, malformed, or
//! permission-denied file is treated as `Unknown` with a `tracing::warn!`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::tools::approval_broker::ThreadKey;

/// File name (inside the zeptoclaw config dir) carrying the per-container
/// `(user_id, agent_id)` pair. Written by the scheduler on first start of a
/// `(user, agent)` sandbox; read by zeptoclaw on gateway startup.
pub const THREAD_META_FILENAME: &str = "thread_meta.json";

/// On-disk shape of `thread_meta.json`. Field names are intentionally
/// `snake_case` to match the rest of zeptoclaw's persisted state. Unknown
/// fields are tolerated (forward-compat) so PR2 can add e.g. `created_at`
/// without breaking old deployments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMetaFile {
    pub user_id: String,
    pub agent_id: String,
}

/// Process-level identity for HITL routing. Constructed once at gateway
/// startup, then cloned (`Arc`) into the `AgentLoop`, approval handler,
/// and inbound interceptor.
#[derive(Debug, Clone)]
pub enum ThreadIdentity {
    Known(ThreadKey),
    Unknown,
}

impl ThreadIdentity {
    /// Attempt to load identity from `<config_dir>/thread_meta.json`.
    ///
    /// Returns `Unknown` (with a warning log) on any error so the gateway
    /// can still start up and serve requests through the legacy fallback
    /// path. The intent: missing meta is a deployment situation, not a
    /// crash.
    pub fn load_from_config_dir(config_dir: &Path) -> Self {
        let path = config_dir.join(THREAD_META_FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<ThreadMetaFile>(&body) {
                Ok(meta) => {
                    if meta.user_id.is_empty() || meta.agent_id.is_empty() {
                        warn!(
                            path = %path.display(),
                            "thread_meta.json has empty user_id or agent_id; \
                             falling back to Unknown"
                        );
                        Self::Unknown
                    } else {
                        debug!(
                            user = %meta.user_id,
                            agent = %meta.agent_id,
                            path = %path.display(),
                            "loaded thread identity"
                        );
                        Self::Known(ThreadKey::new(meta.user_id, meta.agent_id))
                    }
                }
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to parse thread_meta.json; falling back to Unknown"
                    );
                    Self::Unknown
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(
                    path = %path.display(),
                    "thread_meta.json absent; using ThreadIdentity::Unknown (dev/in-process mode)"
                );
                Self::Unknown
            }
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read thread_meta.json; falling back to Unknown"
                );
                Self::Unknown
            }
        }
    }

    /// Resolve a `ThreadKey` for broker routing. When this identity is
    /// `Unknown`, fall back to the `chat_id`-keyed bucket so legacy
    /// callers keep working.
    pub fn resolve_key(&self, chat_id_fallback: &str) -> ThreadKey {
        match self {
            Self::Known(key) => key.clone(),
            Self::Unknown => ThreadKey::from_chat_id_fallback(chat_id_fallback),
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    pub fn as_known(&self) -> Option<&ThreadKey> {
        match self {
            Self::Known(k) => Some(k),
            Self::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_meta(dir: &Path, body: &str) {
        let path = dir.join(THREAD_META_FILENAME);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(body.as_bytes()).expect("write");
    }

    #[test]
    fn loads_valid_meta_as_known() {
        let dir = tempdir();
        write_meta(
            dir.path(),
            r#"{"user_id": "alice", "agent_id": "zeptoclaw"}"#,
        );
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(id.is_known());
        let key = id.as_known().expect("known");
        assert_eq!(key.user, "alice");
        assert_eq!(key.agent, "zeptoclaw");
    }

    #[test]
    fn missing_file_is_unknown() {
        let dir = tempdir();
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(!id.is_known());
    }

    #[test]
    fn malformed_json_falls_back_to_unknown() {
        let dir = tempdir();
        write_meta(dir.path(), "not json");
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(!id.is_known());
    }

    #[test]
    fn empty_field_treated_as_unknown() {
        let dir = tempdir();
        write_meta(dir.path(), r#"{"user_id": "", "agent_id": "zc"}"#);
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(!id.is_known(), "empty user_id should degrade to Unknown");

        write_meta(dir.path(), r#"{"user_id": "alice", "agent_id": ""}"#);
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(!id.is_known(), "empty agent_id should degrade to Unknown");
    }

    #[test]
    fn unknown_fields_tolerated_for_forward_compat() {
        let dir = tempdir();
        write_meta(
            dir.path(),
            r#"{"user_id": "alice", "agent_id": "zc", "created_at": "2026-05-17T03:00:00Z"}"#,
        );
        let id = ThreadIdentity::load_from_config_dir(dir.path());
        assert!(id.is_known());
    }

    #[test]
    fn resolve_key_uses_thread_key_when_known() {
        let id = ThreadIdentity::Known(ThreadKey::new("alice", "zc"));
        let key = id.resolve_key("some-chat-id");
        assert_eq!(key.user, "alice");
        assert_eq!(key.agent, "zc");
    }

    #[test]
    fn resolve_key_falls_back_to_chat_id_when_unknown() {
        let id = ThreadIdentity::Unknown;
        let key = id.resolve_key("discord:guild-1");
        assert_eq!(key.user, "discord:guild-1");
        assert_eq!(key.agent, "");
        assert!(key.is_fallback());
    }
}
