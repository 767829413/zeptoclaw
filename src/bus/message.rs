//! Message types for the ZeptoClaw message bus
//!
//! This module defines the core message types used for communication
//! between channels, agents, and the message bus.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metadata key used to flag an [`OutboundMessage`] as carrying a delivery
/// failure (e.g., LLM provider error) rather than a normal assistant reply.
///
/// Channels that understand the flag (currently the ACP family) surface the
/// content as a protocol-level error; channels that don't simply render the
/// text, which is a sane fallback.
pub const OUTBOUND_ERROR_KEY: &str = "error";

/// Represents an incoming message from a channel (e.g., Telegram, Discord, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// The channel this message came from (e.g., "telegram", "discord")
    pub channel: String,
    /// Unique identifier of the sender
    pub sender_id: String,
    /// Unique identifier of the chat/conversation
    pub chat_id: String,
    /// The text content of the message
    pub content: String,
    /// Media attachments (zero or more)
    pub media: Vec<MediaAttachment>,
    /// Session key for routing (format: "channel:chat_id")
    pub session_key: String,
    /// Additional metadata key-value pairs
    pub metadata: HashMap<String, String>,
}

/// Describes how a given [`OutboundMessage`] participates in a streaming
/// reply. Only ACP-family channels interpret non-`Full` variants today;
/// every other channel is routed only the `Full` variant by design (see
/// `channels/manager.rs::dispatch_outbound` — dispatch is per-channel, not
/// broadcast, so adding a variant here does **not** fan out to Telegram,
/// Discord, etc.).
///
/// Reserved for B4-zc (token-level streaming through ACP): the agent loop
/// emits a sequence of `Chunk` messages per LLM delta, followed by a
/// single `ChunkEnd` to signal "stream complete". The ACP transport turns
/// each `Chunk` into a `session/update(agent_message_chunk)` notification
/// and uses `ChunkEnd` as the cue to close the `session/prompt` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundMessageKind {
    /// A complete assistant reply. This is the historical shape of every
    /// `OutboundMessage` and remains the default — all non-streaming
    /// channels and the legacy non-streaming agent path use this.
    #[default]
    Full,
    /// A streaming delta. `content` carries the incremental text to
    /// append. Does **not** close the caller's pending request.
    Chunk,
    /// Terminal frame for a streaming reply. `content` is conventionally
    /// empty (the full reply has already been transmitted via `Chunk`s).
    /// Signals the ACP transport to write its `session/prompt` response.
    ChunkEnd,
}

impl OutboundMessageKind {
    /// Helper for `#[serde(skip_serializing_if)]` — lets us keep the
    /// on-the-wire JSON identical to pre-B4-zc when nothing cares about
    /// streaming (i.e. when the variant is `Full`). Coupled with
    /// `#[serde(default)]` on the field, this gives full bidirectional
    /// compatibility with payloads serialized by older builds.
    fn is_full(&self) -> bool {
        matches!(self, OutboundMessageKind::Full)
    }
}

/// Represents an outgoing message to be sent via a channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// The channel to send this message through
    pub channel: String,
    /// The chat/conversation to send to
    pub chat_id: String,
    /// The text content to send
    pub content: String,
    /// Optional message ID to reply to
    pub reply_to: Option<String>,
    /// Additional metadata key-value pairs for channel-specific delivery hints
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Streaming participation. Defaults to `Full` for both back-compat
    /// (missing field in older JSON deserializes as `Full`) and for the
    /// overwhelmingly common case.
    #[serde(default, skip_serializing_if = "OutboundMessageKind::is_full")]
    pub kind: OutboundMessageKind,
}

/// Represents a media attachment (image, audio, video, or document)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    /// The type of media
    pub media_type: MediaType,
    /// URL to the media (if hosted remotely)
    pub url: Option<String>,
    /// Raw binary data (if available locally)
    pub data: Option<Vec<u8>>,
    /// Original filename
    pub filename: Option<String>,
    /// Explicit MIME type (e.g., "image/jpeg", "image/png")
    pub mime_type: Option<String>,
}

/// Types of media that can be attached to messages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaType {
    /// Image files (PNG, JPG, GIF, etc.)
    Image,
    /// Audio files (MP3, WAV, OGG, etc.)
    Audio,
    /// Video files (MP4, WebM, etc.)
    Video,
    /// Document files (PDF, DOCX, etc.)
    Document,
}

impl InboundMessage {
    /// Creates a new inbound message with the required fields.
    ///
    /// The session key is automatically generated as "channel:chat_id".
    ///
    /// # Arguments
    /// * `channel` - The source channel (e.g., "telegram")
    /// * `sender_id` - Unique identifier of the message sender
    /// * `chat_id` - Unique identifier of the chat/conversation
    /// * `content` - The text content of the message
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::InboundMessage;
    ///
    /// let msg = InboundMessage::new("telegram", "user123", "chat456", "Hello, bot!");
    /// assert_eq!(msg.session_key, "telegram:chat456");
    /// ```
    pub fn new(channel: &str, sender_id: &str, chat_id: &str, content: &str) -> Self {
        Self {
            channel: channel.to_string(),
            sender_id: sender_id.to_string(),
            chat_id: chat_id.to_string(),
            content: content.to_string(),
            media: Vec::new(),
            session_key: format!("{}:{}", channel, chat_id),
            metadata: HashMap::new(),
        }
    }

    /// Attaches media to the message (builder pattern).
    ///
    /// Multiple calls push additional attachments; calling `.with_media()` twice
    /// results in a message with two attachments.
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::{InboundMessage, MediaAttachment, MediaType};
    ///
    /// let media = MediaAttachment::new(MediaType::Image).with_url("https://example.com/image.png");
    /// let msg = InboundMessage::new("telegram", "user123", "chat456", "Check this out!")
    ///     .with_media(media);
    /// assert!(!msg.media.is_empty());
    /// ```
    pub fn with_media(mut self, media: MediaAttachment) -> Self {
        self.media.push(media);
        self
    }

    /// Adds a metadata key-value pair to the message (builder pattern).
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::InboundMessage;
    ///
    /// let msg = InboundMessage::new("telegram", "user123", "chat456", "Hello")
    ///     .with_metadata("message_id", "12345")
    ///     .with_metadata("is_bot", "false");
    /// assert_eq!(msg.metadata.get("message_id"), Some(&"12345".to_string()));
    /// ```
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Checks if this message has any media attached.
    pub fn has_media(&self) -> bool {
        !self.media.is_empty()
    }
}

impl OutboundMessage {
    /// Creates a new outbound message.
    ///
    /// # Arguments
    /// * `channel` - The target channel (e.g., "telegram")
    /// * `chat_id` - The chat/conversation to send to
    /// * `content` - The text content to send
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::OutboundMessage;
    ///
    /// let msg = OutboundMessage::new("telegram", "chat456", "Hello from the bot!");
    /// assert_eq!(msg.channel, "telegram");
    /// ```
    pub fn new(channel: &str, chat_id: &str, content: &str) -> Self {
        Self {
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            content: content.to_string(),
            reply_to: None,
            metadata: HashMap::new(),
            kind: OutboundMessageKind::Full,
        }
    }

    /// Builder: mark this message as a streaming delta (see
    /// [`OutboundMessageKind::Chunk`]). Only ACP channels act on this;
    /// others ignore it by design.
    pub fn with_kind(mut self, kind: OutboundMessageKind) -> Self {
        self.kind = kind;
        self
    }

    /// Marks this message as an error-carrying outbound. Error-aware channels
    /// (e.g., ACP) translate it into a protocol-level error; other channels
    /// render `content` as plain text.
    pub fn mark_error(&mut self) -> &mut Self {
        self.metadata
            .insert(OUTBOUND_ERROR_KEY.to_string(), "true".to_string());
        self
    }

    /// Returns `true` if this message was marked as an error via
    /// [`Self::mark_error`].
    pub fn is_error(&self) -> bool {
        self.metadata.get(OUTBOUND_ERROR_KEY).map(String::as_str) == Some("true")
    }

    /// Sets the message ID to reply to (builder pattern).
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::OutboundMessage;
    ///
    /// let msg = OutboundMessage::new("telegram", "chat456", "This is a reply")
    ///     .with_reply("original_msg_123");
    /// assert_eq!(msg.reply_to, Some("original_msg_123".to_string()));
    /// ```
    pub fn with_reply(mut self, message_id: &str) -> Self {
        self.reply_to = Some(message_id.to_string());
        self
    }

    /// Adds a metadata key-value pair to the outbound message.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Creates an outbound message as a response to an inbound message.
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::{InboundMessage, OutboundMessage};
    ///
    /// let inbound = InboundMessage::new("telegram", "user123", "chat456", "Hello");
    /// let response = OutboundMessage::reply_to(&inbound, "Hello back!");
    /// assert_eq!(response.channel, "telegram");
    /// assert_eq!(response.chat_id, "chat456");
    /// ```
    pub fn reply_to(msg: &InboundMessage, content: &str) -> Self {
        Self::new(&msg.channel, &msg.chat_id, content)
    }
}

impl MediaAttachment {
    /// Creates a new media attachment of the specified type.
    pub fn new(media_type: MediaType) -> Self {
        Self {
            media_type,
            url: None,
            data: None,
            filename: None,
            mime_type: None,
        }
    }

    /// Sets the URL for the media (builder pattern).
    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    /// Sets the raw binary data (builder pattern).
    pub fn with_data(mut self, data: Vec<u8>) -> Self {
        self.data = Some(data);
        self
    }

    /// Sets the filename (builder pattern).
    pub fn with_filename(mut self, filename: &str) -> Self {
        self.filename = Some(filename.to_string());
        self
    }

    /// Sets the MIME type for the media (builder pattern).
    ///
    /// # Example
    /// ```
    /// use zeptoclaw::bus::message::{MediaAttachment, MediaType};
    ///
    /// let media = MediaAttachment::new(MediaType::Image)
    ///     .with_mime_type("image/webp");
    /// assert_eq!(media.mime_type, Some("image/webp".to_string()));
    /// ```
    pub fn with_mime_type(mut self, mime_type: &str) -> Self {
        self.mime_type = Some(mime_type.to_string());
        self
    }

    /// Checks if the media has a URL.
    pub fn has_url(&self) -> bool {
        self.url.is_some()
    }

    /// Checks if the media has binary data.
    pub fn has_data(&self) -> bool {
        self.data.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inbound_message_creation() {
        let msg = InboundMessage::new("telegram", "user123", "chat456", "Hello");
        assert_eq!(msg.channel, "telegram");
        assert_eq!(msg.sender_id, "user123");
        assert_eq!(msg.chat_id, "chat456");
        assert_eq!(msg.content, "Hello");
        assert_eq!(msg.session_key, "telegram:chat456");
        assert!(msg.media.is_empty());
        assert!(msg.metadata.is_empty());
    }

    #[test]
    fn test_inbound_message_with_media() {
        let media = MediaAttachment::new(MediaType::Image)
            .with_url("https://example.com/image.png")
            .with_filename("image.png");

        let msg =
            InboundMessage::new("discord", "user1", "channel1", "Check this").with_media(media);

        assert!(msg.has_media());
        let attachment = msg.media.first().unwrap();
        assert_eq!(attachment.media_type, MediaType::Image);
        assert_eq!(
            attachment.url,
            Some("https://example.com/image.png".to_string())
        );
        assert_eq!(attachment.filename, Some("image.png".to_string()));
    }

    #[test]
    fn test_inbound_message_with_metadata() {
        let msg = InboundMessage::new("telegram", "user123", "chat456", "Hello")
            .with_metadata("message_id", "12345")
            .with_metadata("timestamp", "2024-01-01T00:00:00Z");

        assert_eq!(msg.metadata.len(), 2);
        assert_eq!(msg.metadata.get("message_id"), Some(&"12345".to_string()));
        assert_eq!(
            msg.metadata.get("timestamp"),
            Some(&"2024-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn test_outbound_message_creation() {
        let msg = OutboundMessage::new("telegram", "chat456", "Response");
        assert_eq!(msg.channel, "telegram");
        assert_eq!(msg.chat_id, "chat456");
        assert_eq!(msg.content, "Response");
        assert!(msg.reply_to.is_none());
        assert!(msg.metadata.is_empty());
    }

    #[test]
    fn test_outbound_message_with_reply() {
        let msg = OutboundMessage::new("telegram", "chat456", "This is a reply")
            .with_reply("original_msg_123");

        assert_eq!(msg.reply_to, Some("original_msg_123".to_string()));
    }

    #[test]
    fn test_outbound_message_with_metadata() {
        let msg = OutboundMessage::new("discord", "channel1", "Hello")
            .with_metadata("discord_thread_name", "Daily Updates")
            .with_metadata("discord_thread_auto_archive_minutes", "60");

        assert_eq!(
            msg.metadata.get("discord_thread_name"),
            Some(&"Daily Updates".to_string())
        );
        assert_eq!(
            msg.metadata.get("discord_thread_auto_archive_minutes"),
            Some(&"60".to_string())
        );
    }

    #[test]
    fn test_outbound_reply_to_inbound() {
        let inbound = InboundMessage::new("telegram", "user123", "chat456", "Hello");
        let response = OutboundMessage::reply_to(&inbound, "Hello back!");

        assert_eq!(response.channel, "telegram");
        assert_eq!(response.chat_id, "chat456");
        assert_eq!(response.content, "Hello back!");
    }

    #[test]
    fn test_media_attachment_creation() {
        let media = MediaAttachment::new(MediaType::Audio)
            .with_url("https://example.com/audio.mp3")
            .with_data(vec![1, 2, 3, 4])
            .with_filename("audio.mp3");

        assert_eq!(media.media_type, MediaType::Audio);
        assert!(media.has_url());
        assert!(media.has_data());
        assert_eq!(media.filename, Some("audio.mp3".to_string()));
    }

    #[test]
    fn test_media_type_equality() {
        assert_eq!(MediaType::Image, MediaType::Image);
        assert_ne!(MediaType::Image, MediaType::Audio);
        assert_ne!(MediaType::Video, MediaType::Document);
    }

    #[test]
    fn test_inbound_message_with_multiple_media() {
        let media1 = MediaAttachment::new(MediaType::Image)
            .with_data(vec![0xFF, 0xD8])
            .with_mime_type("image/jpeg");
        let media2 = MediaAttachment::new(MediaType::Image)
            .with_data(vec![0x89, 0x50])
            .with_mime_type("image/png");

        let msg = InboundMessage::new("telegram", "user1", "chat1", "Two images")
            .with_media(media1)
            .with_media(media2);

        assert_eq!(msg.media.len(), 2);
        assert!(msg.has_media());
    }

    #[test]
    fn test_media_attachment_mime_type() {
        let media = MediaAttachment::new(MediaType::Image).with_mime_type("image/webp");
        assert_eq!(media.mime_type, Some("image/webp".to_string()));
    }

    #[test]
    fn test_message_serialization() {
        let msg = InboundMessage::new("telegram", "user123", "chat456", "Hello")
            .with_metadata("key", "value");

        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        let deserialized: InboundMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(deserialized.channel, "telegram");
        assert_eq!(deserialized.content, "Hello");
        assert_eq!(deserialized.metadata.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_outbound_message_serialization() {
        let msg = OutboundMessage::new("discord", "channel1", "Hello Discord!")
            .with_reply("msg_123")
            .with_metadata("discord_thread_name", "ops-thread");

        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        let deserialized: OutboundMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(deserialized.channel, "discord");
        assert_eq!(deserialized.reply_to, Some("msg_123".to_string()));
        assert_eq!(
            deserialized.metadata.get("discord_thread_name"),
            Some(&"ops-thread".to_string())
        );
    }

    // ---- B4-zc wire compatibility ----
    //
    // The `kind` field was added in B4-zc for ACP streaming. Everything
    // that already existed must continue to serialize and deserialize
    // unchanged: older builds must not choke on payloads from newer
    // builds (newer payloads omit `kind` when it's `Full`), and newer
    // builds must not choke on older payloads (missing `kind` means
    // `Full` by default).

    #[test]
    fn outbound_default_kind_is_full() {
        let msg = OutboundMessage::new("discord", "c1", "hi");
        assert_eq!(msg.kind, OutboundMessageKind::Full);
    }

    #[test]
    fn outbound_full_kind_is_omitted_from_json() {
        // If we ever serialize `"kind":"full"` into the wire payload,
        // older receivers without the field would also accept it (serde
        // default kicks in), but the point is to keep the on-the-wire
        // shape byte-identical to pre-B4-zc for the common case. Assert
        // the `kind` key simply isn't there.
        let msg = OutboundMessage::new("discord", "c1", "hi");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(
            !json.contains("\"kind\""),
            "Full kind must not appear in serialized JSON; got: {json}"
        );
    }

    #[test]
    fn outbound_missing_kind_deserializes_as_full() {
        // JSON from an older producer — no `kind` field.
        let legacy =
            r#"{"channel":"discord","chat_id":"c1","content":"hi","reply_to":null}"#;
        let msg: OutboundMessage = serde_json::from_str(legacy).expect("deserialize");
        assert_eq!(msg.kind, OutboundMessageKind::Full);
    }

    #[test]
    fn outbound_chunk_kind_roundtrips() {
        let msg = OutboundMessage::new("acp", "sess1", "tok")
            .with_kind(OutboundMessageKind::Chunk);
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"kind\":\"chunk\""), "json: {json}");
        let back: OutboundMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.kind, OutboundMessageKind::Chunk);
        assert_eq!(back.content, "tok");
    }

    #[test]
    fn outbound_chunk_end_kind_roundtrips() {
        let msg = OutboundMessage::new("acp", "sess1", "")
            .with_kind(OutboundMessageKind::ChunkEnd);
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"kind\":\"chunk_end\""), "json: {json}");
        let back: OutboundMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.kind, OutboundMessageKind::ChunkEnd);
    }
}
