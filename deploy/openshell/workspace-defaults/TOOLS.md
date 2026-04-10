# Tool Reference

## Xiaohongshu (小红书) — via MCP

The `xiaohongshu` MCP server provides tools for interacting with Xiaohongshu.

### Available MCP Tools

| Tool | Description |
|------|-------------|
| `get_qr_code` | Get login QR code — returns base64 image |
| `check_login_status` | Check if QR code has been scanned |
| `search_notes` | Search notes by keyword |
| `get_note_detail` | Get full note content by URL |
| `publish_image_note` | Publish an image note |
| `publish_video_note` | Publish a video note |

### Login Flow

1. Call `get_qr_code` → sends QR image
2. User scans with Xiaohongshu app
3. Poll `check_login_status` until success
4. Cookies are persisted — login survives restarts

### Publishing Notes

Images must be **HTTPS URLs** (e.g. `https://images.unsplash.com/photo-xxx`).
The MCP server downloads them automatically. Do not use local file paths.

```json
{
  "title": "标题",
  "content": "正文内容\n\n#标签1 #标签2",
  "images": ["https://example.com/photo1.jpg", "https://example.com/photo2.jpg"]
}
```

### Error Handling

| Error | Action |
|-------|--------|
| Login expired | Re-run login flow |
| Image download failed | Verify URL is accessible, retry |
| Publish timeout | Wait 30s, retry once |
