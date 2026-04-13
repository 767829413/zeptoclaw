#!/usr/bin/env bash
set -euo pipefail

log() { echo "[zeptoclaw-start] $(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }

# ---------------------------------------------------------------------------
# Secure credential injection.
#
# OpenShell injects openshell:resolve:env: placeholders into env vars.
# The proxy resolves them in HTTP headers/query/path, but NOT in request
# bodies, WebSocket frames, or other non-HTTP protocols.
#
# To support services that need credentials outside HTTP headers (Discord
# WebSocket IDENTIFY, gRPC metadata, etc.), runtime.sh uploads a _creds
# file with real values.  We read them into shell-local variables (_SEC_*)
# so that env vars remain as placeholders -- nothing leaks to
# /proc/self/environ.
# ---------------------------------------------------------------------------

CREDS_FILE=""

# Check hostPath mount first (persistent volume), then legacy upload path
for _creds_candidate in "/sandbox/.zeptoclaw/_creds" "/sandbox/_creds"; do
    if [ -d "${_creds_candidate}" ]; then
        CREDS_FILE=$(find "${_creds_candidate}" -maxdepth 1 -type f -print -quit 2>/dev/null)
        break
    elif [ -f "${_creds_candidate}" ]; then
        CREDS_FILE="${_creds_candidate}"
        break
    fi
done

if [ -n "${CREDS_FILE}" ] && [ -f "${CREDS_FILE}" ]; then
    while IFS='=' read -r _key _val; do
        [[ "${_key}" =~ ^[A-Z_][A-Z0-9_]*$ ]] || continue
        printf -v "_SEC_${_key}" '%s' "${_val}"
    done < "${CREDS_FILE}"
    rm -f "${CREDS_FILE}"
    log "Loaded credentials from ${CREDS_FILE} (env vars unchanged)"
else
    log "WARN: credentials not found — falling back to env vars (may contain placeholders)"
fi

_cred() {
    local sec_var="_SEC_$1"
    printf '%s' "${!sec_var:-${!1:-}}"
}

CONFIG_DIR="${HOME}/.zeptoclaw"
CONFIG_FILE="${CONFIG_DIR}/config.json"

mkdir -p "${CONFIG_DIR}/workspace" \
         "${CONFIG_DIR}/sessions" \
         "${CONFIG_DIR}/logs" \
         "${CONFIG_DIR}/cron" \
         "${CONFIG_DIR}/memory" \
         "${CONFIG_DIR}/workspace/skills"

# --- Fast path: reuse existing config on pod restart ---
# If config.json already exists (from a previous run on the persistent volume)
# and credentials are not available, skip regeneration and go straight to exec.
if [ -f "${CONFIG_FILE}" ] && [ -z "${CREDS_FILE}" ]; then
    log "Reusing existing ${CONFIG_FILE} (no new credentials)"
    exec zeptoclaw gateway
fi

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
        cp -rn "${DEFAULTS_DIR}/skills/"* "${CONFIG_DIR}/workspace/skills/" 2>/dev/null || true
        SKILL_COUNT=$(find "${CONFIG_DIR}/workspace/skills" -name "SKILL.md" 2>/dev/null | wc -l)
        log "Skills directory: ${SKILL_COUNT} skill(s) available"
    fi
fi

# --- Build provider config from env ---
PROVIDERS="{}"

_val=$(_cred ANTHROPIC_API_KEY)
if [ -n "$_val" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$_val" '. + {anthropic: {api_key: $k}}')
    log "Provider: anthropic"
fi

_val=$(_cred OPENAI_API_KEY)
if [ -n "$_val" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$_val" '. + {openai: {api_key: $k}}')
    log "Provider: openai"
fi

_val=$(_cred OPENROUTER_API_KEY)
if [ -n "$_val" ]; then
    PROVIDERS=$(echo "$PROVIDERS" | jq --arg k "$_val" '. + {openrouter: {api_key: $k}}')
    log "Provider: openrouter"
fi

_azure_ep=$(_cred AZURE_OPENAI_ENDPOINT)
_azure_key=$(_cred AZURE_OPENAI_API_KEY)
if [ -n "$_azure_ep" ] && [ -n "$_azure_key" ]; then
    _azure_ver=$(_cred ZEPTOCLAW_PROVIDERS_AZURE_API_VERSION)
    _azure_ver="${_azure_ver:-2025-04-01-preview}"
    _azure_auth=$(_cred ZEPTOCLAW_PROVIDERS_AZURE_AUTH_HEADER)
    _azure_auth="${_azure_auth:-api-key}"
    PROVIDERS=$(echo "$PROVIDERS" | jq \
        --arg base "$_azure_ep" \
        --arg key "$_azure_key" \
        --arg ver "$_azure_ver" \
        --arg auth "$_azure_auth" \
        '. + {azure: {api_key: $key, api_base: $base, api_version: $ver, auth_header: $auth}}')
    log "Provider: azure (auth_header=${_azure_auth})"
fi

# --- Build channel config from env ---
CHANNELS="{}"

_val=$(_cred DISCORD_BOT_TOKEN)
if [ -n "$_val" ]; then
    CHANNELS=$(echo "$CHANNELS" | jq --arg t "$_val" '. + {discord: {enabled: true, token: $t}}')
    log "Channel: discord"
fi

_val=$(_cred TELEGRAM_BOT_TOKEN)
if [ -n "$_val" ]; then
    CHANNELS=$(echo "$CHANNELS" | jq --arg t "$_val" '. + {telegram: {enabled: true, token: $t}}')
    log "Channel: telegram"
fi

# --- Assemble config ---
cat > "${CONFIG_FILE}" <<EOJSON
{
  "providers": ${PROVIDERS},
  "channels": ${CHANNELS},
  "runtime": {
    "runtime_type": "native",
    "allow_fallback_to_native": false
  },
  "agents": {
    "defaults": {
      "workspace": "${CONFIG_DIR}/workspace"$(
        _m=$(_cred ZEPTOCLAW_AGENTS_DEFAULTS_MODEL)
        [ -n "$_m" ] && printf ',\n      "model": "%s"' "$_m"
      )
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

chmod 600 "${CONFIG_FILE}"

# --- Merge MCP config if present ---
MCP_SERVERS_FILE="/etc/zeptoclaw/mcp-servers.json"
if [ -f "$MCP_SERVERS_FILE" ] && jq empty "$MCP_SERVERS_FILE" 2>/dev/null; then
    MCP_SECTION=$(jq -c '.' "$MCP_SERVERS_FILE")
    TMP_CONFIG=$(mktemp)
    jq --argjson mcp "$MCP_SECTION" '. + {mcp: $mcp}' "${CONFIG_FILE}" > "$TMP_CONFIG"
    mv "$TMP_CONFIG" "${CONFIG_FILE}"
    chmod 600 "${CONFIG_FILE}"
    MCP_COUNT=$(jq '.servers | length' "$MCP_SERVERS_FILE" 2>/dev/null || echo "?")
    log "Merged ${MCP_COUNT} MCP server(s)"
fi

log "Config written to ${CONFIG_FILE}"
log "Runtime: native (OpenShell sandbox provides isolation)"

# Unset Azure env vars that Rust's apply_env_overrides() would read.
# In OpenShell, these contain proxy placeholders (openshell:resolve:env:...)
# which work for HTTP header injection but NOT for URL construction.
# The real values are already baked into config.json above.
unset AZURE_OPENAI_ENDPOINT AZURE_OPENAI_API_KEY 2>/dev/null || true
unset ZEPTOCLAW_PROVIDERS_AZURE_API_KEY ZEPTOCLAW_PROVIDERS_AZURE_API_BASE 2>/dev/null || true
unset ZEPTOCLAW_PROVIDERS_AZURE_API_VERSION ZEPTOCLAW_PROVIDERS_AZURE_AUTH_HEADER 2>/dev/null || true

exec zeptoclaw gateway
