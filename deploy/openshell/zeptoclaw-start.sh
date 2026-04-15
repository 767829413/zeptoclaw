#!/usr/bin/env bash
set -euo pipefail

log() { echo "[zeptoclaw-start] $(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }

CONFIG_DIR="${HOME}/.zeptoclaw"
CONFIG_FILE="${CONFIG_DIR}/config.json"

mkdir -p "${CONFIG_DIR}"/{workspace,sessions,logs,cron,memory,workspace/skills}

# Copy workspace bootstrap files from image defaults (first start only).
DEFAULTS_DIR="/etc/zeptoclaw/workspace-defaults"
if [ -d "$DEFAULTS_DIR" ]; then
    for f in IDENTITY.md AGENTS.md SOUL.md USER.md TOOLS.md MEMORY.md; do
        if [ -f "${DEFAULTS_DIR}/${f}" ] && [ ! -f "${CONFIG_DIR}/workspace/${f}" ]; then
            cp "${DEFAULTS_DIR}/${f}" "${CONFIG_DIR}/workspace/${f}"
            log "Initialized workspace/${f}"
        fi
    done
    if [ -d "${DEFAULTS_DIR}/skills" ]; then
        cp -rn "${DEFAULTS_DIR}/skills/"* "${CONFIG_DIR}/workspace/skills/" 2>/dev/null || true
    fi
fi

# config.json is generated on the host by runtime.sh write_config()
# and uploaded via --upload at sandbox creation time.
if [ ! -f "${CONFIG_FILE}" ]; then
    log "FATAL: ${CONFIG_FILE} not found. runtime.sh should generate and upload it."
    exit 1
fi

log "Using ${CONFIG_FILE}"
exec zeptoclaw gateway
