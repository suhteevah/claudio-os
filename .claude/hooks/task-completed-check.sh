#!/usr/bin/env bash
# TaskCompleted hook for ClaudioOS
# Vetoes Claude marking a task as done if `cargo check` fails on the workspace.
# Exit code 2 sends the failure message back to Claude so it keeps working.
#
# Skips the check if no .rs files were modified in the session (avoid blocking
# pure documentation/config tasks).

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 0

# Quick fast-path: only enforce if there are .rs files in git status
# (uncommitted changes mean we should validate)
if ! git status --porcelain 2>/dev/null | grep -q '\.rs$'; then
    # No Rust files touched, allow completion silently
    exit 0
fi

# Run cargo check, capture output
CHECK_OUTPUT=$(cargo check --workspace 2>&1)
CHECK_EXIT=$?

if [ $CHECK_EXIT -ne 0 ]; then
    # Build failed — emit a clear message and exit 2 to block completion
    echo "TaskCompleted hook BLOCKED: \`cargo check --workspace\` failed." >&2
    echo "" >&2
    echo "Last 30 lines of cargo output:" >&2
    echo "$CHECK_OUTPUT" | tail -30 >&2
    echo "" >&2
    echo "Fix the build errors before marking this task complete." >&2
    exit 2
fi

# Build passed, allow completion
exit 0
