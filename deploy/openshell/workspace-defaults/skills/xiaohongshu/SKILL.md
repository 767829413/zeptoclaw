# Skill: Xiaohongshu (小红书)

Publish and search content on Xiaohongshu via MCP tools.

## Prerequisites

- Xiaohongshu MCP server running (sidecar container)
- User must be logged in (cookies persisted)

## Login Flow

1. Call MCP tool `get_qr_code`
2. Send the returned QR image to user
3. User scans with Xiaohongshu app
4. Poll `check_login_status` (max 60s, interval 3s)
5. On success: cookies saved, subsequent calls authenticated

## Publish Image Note

```
MCP tool: publish_image_note
Params:
  title: string (required)
  content: string (required, supports #hashtags)
  images: string[] (HTTPS URLs only, 1-9 images)
```

Key rules:
- Images MUST be HTTPS URLs — the MCP server downloads them
- Never use local file paths
- Content supports newlines and #hashtags
- Wait for publish confirmation before reporting success

## Search Notes

```
MCP tool: search_notes
Params:
  keyword: string
```

Returns list of notes with titles, links, and previews.

## Get Note Detail

```
MCP tool: get_note_detail
Params:
  url: string (xiaohongshu note URL)
```

Returns full note content including text, images, and engagement metrics.

## Error Recovery

| Situation | Action |
|-----------|--------|
| "not logged in" / cookie expired | Re-run login flow |
| Image URL 404 | Ask user for working URL |
| Publish fails | Wait 30s, retry once |
| Browser crash | MCP container auto-restarts |
