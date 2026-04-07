#!/usr/bin/env bash
# CwdChanged hook for ClaudioOS
# Logs every directory change Claude makes to .claude/cwd-history.log
# for observability and post-session debugging.
#
# Cannot block (CwdChanged is observability-only per Claude Code docs).

set -uo pipefail

LOG_FILE="$(dirname "$0")/../cwd-history.log"
TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

# Hook env vars: CLAUDE_OLD_CWD and CLAUDE_NEW_CWD (or read from stdin payload)
OLD_CWD="${CLAUDE_OLD_CWD:-?}"
NEW_CWD="${CLAUDE_NEW_CWD:-$(pwd)}"

echo "[$TIMESTAMP] $OLD_CWD -> $NEW_CWD" >> "$LOG_FILE"

exit 0
