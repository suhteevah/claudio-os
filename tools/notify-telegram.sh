#!/usr/bin/env bash
# notify-telegram.sh — send a message to a Telegram chat via Bot API.
#
# Reads TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID from .claude/.env
# (gitignored). Falls back to environment if .env is missing.
#
# Usage:
#   bash tools/notify-telegram.sh "training done: 7B run finished, loss=0.31"
#   python tools/fine-tune.py && bash tools/notify-telegram.sh "7B finished"
#   python tools/training-watchdog.py | bash tools/notify-telegram.sh -
#
# A "-" argument reads the message from stdin.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$REPO_ROOT/.claude/.env"

if [[ -f "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$ENV_FILE"; set +a
fi

: "${TELEGRAM_BOT_TOKEN:?TELEGRAM_BOT_TOKEN not set — put it in .claude/.env}"
: "${TELEGRAM_CHAT_ID:?TELEGRAM_CHAT_ID not set — put it in .claude/.env}"

if [[ "${1:-}" == "-" ]]; then
    MSG="$(cat)"
elif [[ $# -gt 0 ]]; then
    MSG="$*"
else
    echo "usage: $0 <message> | $0 - (read stdin)" >&2
    exit 64
fi

# Telegram caps messages at 4096 chars; truncate with marker.
if [[ ${#MSG} -gt 4000 ]]; then
    MSG="${MSG:0:3950}

...[truncated, ${#MSG} chars total]"
fi

# Prefix with hostname so multi-machine pings are distinguishable.
HOST="$(hostname)"
FULL_MSG="[$HOST] $MSG"

# Build JSON body in a temp file via Python (preserves UTF-8 across the
# Win32 cmdline boundary, where curl.exe would otherwise transcode args
# from UTF-8 to cp1252 and Telegram rejects with 400 "must be UTF-8").
BODY_FILE="$(mktemp -t telegram-body.XXXXXX.json)"
trap 'rm -f "$BODY_FILE"' EXIT
MSG="$FULL_MSG" CHAT="$TELEGRAM_CHAT_ID" python -c '
import json, os, sys
sys.stdout.buffer.write(
    json.dumps(
        {"chat_id": int(os.environ["CHAT"]), "text": os.environ["MSG"]},
        ensure_ascii=False,
    ).encode("utf-8")
)
' > "$BODY_FILE"

curl -sS --fail-with-body \
    -H "Content-Type: application/json" \
    --data-binary "@${BODY_FILE}" \
    "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
    > /dev/null

echo "telegram: sent (${#FULL_MSG} chars)"
