#!/usr/bin/env bash
set -euo pipefail

log() { echo "[zeptoclaw-start] $(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }

CONFIG_DIR="${HOME}/.zeptoclaw"
CONFIG_FILE="${CONFIG_DIR}/config.json"

mkdir -p "${CONFIG_DIR}/workspace" \
         "${CONFIG_DIR}/sessions" \
         "${CONFIG_DIR}/logs" \
         "${CONFIG_DIR}/cron" \
         "${CONFIG_DIR}/memory" \
         "${HOME}/skills"

# --- Populate workspace bootstrap files (only if not already present) ---
DEFAULTS_DIR="/etc/zeptoclaw/workspace-defaults"
if [ -d "$DEFAULTS_DIR" ]; then
    for f in IDENTITY.md AGENTS.md SOUL.md USER.md TOOLS.md; do
        if [ -f "${DEFAULTS_DIR}/${f}" ] && [ ! -f "${CONFIG_DIR}/workspace/${f}" ]; then
            cp "${DEFAULTS_DIR}/${f}" "${CONFIG_DIR}/workspace/${f}"
            log "Initialized workspace/${f} from defaults"
        fi
    done
    if [ -d "${DEFAULTS_DIR}/skills" ]; then
        cp -rn "${DEFAULTS_DIR}/skills/"* "${HOME}/skills/" 2>/dev/null || true
        SKILL_COUNT=$(find "${HOME}/skills" -name "SKILL.md" 2>/dev/null | wc -l)
        log "Skills directory: ${SKILL_COUNT} skill(s) available"
    fi
fi

# --- Build provider config from env ---
PROVIDERS="{}"

if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$ANTHROPIC_API_KEY" '. + {anthropic: {api_key: $k}}')
    log "Provider: anthropic (from env)"
fi

if [ -n "${OPENAI_API_KEY:-}" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$OPENAI_API_KEY" '. + {openai: {api_key: $k}}')
    log "Provider: openai (from env)"
fi

if [ -n "${OPENROUTER_API_KEY:-}" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$OPENROUTER_API_KEY" '. + {openrouter: {api_key: $k}}')
    log "Provider: openrouter (from env)"
fi

if [ -n "${AZURE_OPENAI_ENDPOINT:-}" ] && [ -n "${AZURE_OPENAI_BEARER_TOKEN:-}" ]; then
    AZURE_VER="${ZEPTOCLAW_PROVIDERS_AZURE_API_VERSION:-2025-04-01-preview}"
    PROVIDERS=$(echo "$PROVIDERS" | jq \
        --arg base "$AZURE_OPENAI_ENDPOINT" \
        --arg key "$AZURE_OPENAI_BEARER_TOKEN" \
        --arg ver "$AZURE_VER" \
        '. + {azure: {api_key: $key, api_base: $base, api_version: $ver}}')
    log "Provider: azure (from env)"
fi

# --- Build channel config from env ---
CHANNELS="{}"

if [ -n "${DISCORD_BOT_TOKEN:-}" ]; then
    CHANNELS=$(echo "$CHANNELS" | jq --arg t "$DISCORD_BOT_TOKEN" '. + {discord: {enabled: true, bot_token: $t}}')
    log "Channel: discord (long-polling)"
fi

if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
    CHANNELS=$(echo "$CHANNELS" | jq --arg t "$TELEGRAM_BOT_TOKEN" '. + {telegram: {enabled: true, bot_token: $t}}')
    log "Channel: telegram (long-polling)"
fi

# --- Assemble config ---
cat > "${CONFIG_FILE}" <<EOJSON
{
  "providers": ${PROVIDERS},
  "channels": ${CHANNELS},
  "runtime": {
    "runtime_type": "Native",
    "allow_fallback_to_native": false
  },
  "agents": {
    "defaults": {
      "workspace": "${CONFIG_DIR}/workspace"
    }
  },
  "gateway": {
    "host": "0.0.0.0",
    "port": 8080,
    "startup_guard": {
      "enabled": true,
      "max_crashes": 3,
      "window_secs": 300
    }
  },
  "tools": {
    "shell": {
      "enabled": true
    }
  }
}
EOJSON

# --- Merge MCP config if present ---
MCP_SERVERS_FILE="/etc/zeptoclaw/mcp-servers.json"
if [ -f "$MCP_SERVERS_FILE" ] && jq empty "$MCP_SERVERS_FILE" 2>/dev/null; then
    MCP_SECTION=$(jq -c '.' "$MCP_SERVERS_FILE")
    TMP_CONFIG=$(mktemp)
    jq --argjson mcp "$MCP_SECTION" '. + {mcp: $mcp}' "${CONFIG_FILE}" > "$TMP_CONFIG"
    mv "$TMP_CONFIG" "${CONFIG_FILE}"
    MCP_COUNT=$(jq '.servers | length' "$MCP_SERVERS_FILE" 2>/dev/null || echo "?")
    log "Merged ${MCP_COUNT} MCP server(s)"
fi

log "Config written to ${CONFIG_FILE}"
log "Runtime: native (OpenShell sandbox provides isolation)"

exec zeptoclaw gateway
