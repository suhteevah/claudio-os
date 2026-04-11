---
name: handoff
description: End-of-session handoff — update HANDOFF.md, sync memory files, commit and push. Use at the end of any work session or when the user says "handoff", "wrap up", "save state", or "I'm done for now".
user_invocable: true
---

# Session Handoff

Perform these steps in order:

## 1. Update HANDOFF.md
Write or update `HANDOFF.md` in the repo root with:
- **Date** (today's date)
- **What was completed** this session (bullet list, be specific)
- **Current state** (what works, what's broken, what's half-done)
- **Blockers** (anything preventing progress)
- **Next steps** (what the next session should pick up)
- **Uncommitted changes** (list modified files if any)

## 2. Update memory files
Update the relevant memory file(s) in the Claude memory directory:
- If the handoff file exists, update it with current state
- If project context changed significantly, update project memory
- Remove stale information that's no longer true

## 3. Commit and push
- `git add` the changed files (HANDOFF.md + any work from this session)
- Commit with a descriptive message summarizing the session's work
- Push to remote
- If there are files the user might not want committed (large models, temp files), list them and ask first

## 4. Report
Tell the user:
- What was committed and pushed
- Any manual actions still required (e.g., "run the training overnight", "install X before next session")
- Any files left uncommitted and why
