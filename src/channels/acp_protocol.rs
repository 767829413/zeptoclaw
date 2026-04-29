//! ACP (Agent Client Protocol) JSON-RPC and method types.
//!
//! Standard methods: initialize, session/new, session/prompt, session/cancel, session/update,
//! session/list (optional, gated by sessionCapabilities.list).
//! See https://agentclientprotocol.com/protocol/overview

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// JSON-RPC 2.0 reserved server-error code (range -32000..-32099). We use the
/// top of the range to surface application-level failures (e.g., LLM provider
/// errors propagated from the agent) on in-flight `session/prompt` requests.
pub const JSONRPC_SERVER_ERROR: i64 = -32000;

/// JSON-RPC 2.0 request (method call with optional id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// --- initialize ---

/// initialize request params (minimal).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: serde_json::Value,
    #[serde(rename = "clientCapabilities", default)]
    pub client_capabilities: Option<serde_json::Value>,
    #[serde(rename = "clientInfo", skip_serializing_if = "Option::is_none")]
    pub client_info: Option<ClientInfo>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: Option<String>,
    pub title: Option<String>,
    pub version: Option<String>,
}

/// Generate a new unique identifier for ACP sessions and client tokens.
///
/// All ACP ID generation goes through this function so the scheme can be
/// swapped project-wide by changing only this one place.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Convert a slice of prompt content blocks into a flat text string.
///
/// `Text` blocks are joined with newlines; `ResourceLink` blocks become a
/// `[Resource: name (uri)]` placeholder. All other block types are skipped.
pub fn prompt_blocks_to_text(blocks: &[PromptContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            PromptContentBlock::Text { text } => parts.push(text.clone()),
            PromptContentBlock::ResourceLink { uri, name, .. } => {
                parts.push(format!("[Resource: {} ({})]", name, uri));
            }
            _ => {}
        }
    }
    parts.join("\n").trim().to_string()
}

/// initialize result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: serde_json::Value,
    #[serde(rename = "agentCapabilities")]
    pub agent_capabilities: AgentCapabilities,
    #[serde(rename = "agentInfo", skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<AgentInfo>,
    #[serde(rename = "authMethods", default)]
    pub auth_methods: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    #[serde(rename = "loadSession", skip_serializing_if = "Option::is_none")]
    pub load_session: Option<bool>,
    #[serde(rename = "promptCapabilities", skip_serializing_if = "Option::is_none")]
    pub prompt_capabilities: Option<serde_json::Value>,
    #[serde(rename = "mcpCapabilities", skip_serializing_if = "Option::is_none")]
    pub mcp_capabilities: Option<serde_json::Value>,
    #[serde(
        rename = "sessionCapabilities",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_capabilities: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
}

// --- session/new ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNewParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNewResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

// --- session/prompt ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPromptParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub prompt: Vec<PromptContentBlock>,
}

/// A content block that may appear in a `session/prompt` request.
///
/// All agents MUST support `Text` and `ResourceLink`. `Image`, `Audio`, and
/// `Resource` (embedded) are optional and gated by prompt capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptContentBlock {
    /// Plain text — MUST be supported by all agents.
    Text { text: String },
    /// Embedded resource contents (requires `embeddedContext` capability).
    Resource { resource: serde_json::Value },
    /// Image data (requires `image` capability).
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// Optional URI reference for the image source.
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    /// Audio data (requires `audio` capability).
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// Resource link — MUST be supported by all agents.
    ResourceLink {
        uri: String,
        name: String,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    /// Unknown/future content type — silently ignored.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPromptResult {
    #[serde(rename = "stopReason")]
    pub stop_reason: String,
}

// --- session/list ---

/// session/list request params (cwd filter and cursor are parsed but not yet applied).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// session/list result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
    /// Pagination cursor for the next page (null when no more pages).
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Per-session metadata returned by session/list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The session identifier.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Working directory for the session (from session/new params, or empty string if not set).
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Extension metadata (ZeptoClaw sets `pending: bool` to indicate an in-flight prompt).
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

// --- session/cancel (notification) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCancelParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

// --- session/update (notification from agent to client) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdateParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub update: SessionUpdatePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdatePayload {
    #[serde(rename = "sessionUpdate")]
    pub session_update: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentBlock>,
    #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Extension: logical name for app-defined custom updates when
    /// `session_update == "agent_custom"`.
    #[serde(rename = "customName", skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// Extension: JSON payload for app-defined custom updates when
    /// `session_update == "agent_custom"`.
    #[serde(rename = "customPayload", skip_serializing_if = "Option::is_none")]
    pub custom_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: Some(text.into()),
        }
    }
}

/// Strip the `"Error: "` prefix that the agent loop prepends to outbound
/// content for plain-text channels (Telegram, etc.). On ACP transports the
/// failure is already represented by the JSON-RPC error envelope, so the
/// prefix is redundant noise inside `error.message`.
pub fn strip_agent_error_prefix(s: &str) -> &str {
    s.strip_prefix("Error: ").unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_agent_error_prefix_removes_prefix_once() {
        assert_eq!(
            strip_agent_error_prefix("Error: Provider error: timeout"),
            "Provider error: timeout"
        );
    }

    #[test]
    fn strip_agent_error_prefix_preserves_non_prefixed() {
        assert_eq!(strip_agent_error_prefix("plain text"), "plain text");
        // Only the literal "Error: " prefix is stripped; case/spacing variants
        // are passed through unchanged.
        assert_eq!(strip_agent_error_prefix("error: lower"), "error: lower");
        assert_eq!(strip_agent_error_prefix("Error:no-space"), "Error:no-space");
    }

    #[test]
    fn session_update_payload_agent_custom_roundtrip() {
        let payload = SessionUpdatePayload {
            session_update: "agent_custom".to_string(),
            content: None,
            tool_call_id: None,
            title: None,
            kind: None,
            status: None,
            custom_name: Some("ui:approval_request".to_string()),
            custom_payload: Some(json!({"requestId":"01JTEST","decision":"approve"})),
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["sessionUpdate"], "agent_custom");
        assert_eq!(v["customName"], "ui:approval_request");
        assert!(v.get("content").is_none());
        let back: SessionUpdatePayload = serde_json::from_value(v).unwrap();
        assert_eq!(back.custom_name.as_deref(), Some("ui:approval_request"));
        assert_eq!(
            back.custom_payload.as_ref().and_then(|p| p.get("decision")),
            Some(&json!("approve"))
        );
    }

    #[test]
    fn session_update_payload_agent_message_omits_custom_fields() {
        let payload = SessionUpdatePayload {
            session_update: "agent_message_chunk".to_string(),
            content: Some(ContentBlock::text("hi")),
            tool_call_id: None,
            title: None,
            kind: None,
            status: None,
            custom_name: None,
            custom_payload: None,
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("customName").is_none());
        assert!(v.get("customPayload").is_none());
        let back: SessionUpdatePayload = serde_json::from_value(v).unwrap();
        assert_eq!(back.session_update, "agent_message_chunk");
        assert!(back.custom_name.is_none());
        assert!(back.custom_payload.is_none());
    }
}
