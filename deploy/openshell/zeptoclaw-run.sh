#!/bin/bash
# Wrapper: run zeptoclaw-start with auto-restart and crash recovery.
# If zeptoclaw crashes, retry up to MAX_RESTARTS times then exit.
# Output goes to stdout/stderr — the host-side supervisor captures it.
# No sleep infinity: the supervisor handles data recovery and sandbox recreation.

MAX_RESTARTS=5
count=0

while [ $count -lt $MAX_RESTARTS ]; do
    zeptoclaw-start 2>&1
    exit_code=$?
    count=$((count + 1))
    echo "[zeptoclaw-run] $(date -u '+%Y-%m-%dT%H:%M:%SZ') exited ${exit_code}, restart ${count}/${MAX_RESTARTS}" >&2
    sleep 5
done

echo "[zeptoclaw-run] $(date -u '+%Y-%m-%dT%H:%M:%SZ') max restarts reached, exiting" >&2
exit 1
