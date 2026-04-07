#!/usr/bin/env bash
# FileChanged hook for ClaudioOS
# Fires when Cargo.toml or Cargo.lock changes.
# Triggers a background `cargo check --workspace` so the next compile is warm.
#
# Background-only — does not block Claude or produce blocking output.

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 0

LOG_FILE=".claude/cargo-warmup.log"
TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

# Run in background so we don't block Claude's loop
(
    echo "[$TIMESTAMP] Cargo dependency file changed, warming build cache..." >> "$LOG_FILE"
    cargo check --workspace >> "$LOG_FILE" 2>&1
    echo "[$TIMESTAMP] Warmup done." >> "$LOG_FILE"
) &

exit 0
