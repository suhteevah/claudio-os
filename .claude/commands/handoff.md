---
name: handoff
description: End-of-session routine — write HANDOFF.md, update memory, commit, push
---

# /handoff — End-of-session routine

Perform the complete end-of-session handoff for the current project:

1. **Write/update HANDOFF.md** in the project root with:
   - `## Last Updated` — today's date
   - `## Project Status` — one-line status with emoji (🟢 working, 🟡 in progress, 🔴 blocked)
   - `## What Was Done This Session` — specifics: files changed, bugs fixed, features shipped, discoveries made
   - `## Current State` — what's working, what's broken, what's stubbed
   - `## Blocking Issues` — anything preventing progress
   - `## What's Next` — prioritized next steps for the incoming session
   - `## Notes for Next Session` — anything the next Claude needs to know that isn't obvious from the code

2. **Update project memory** in `~/.claude/projects/<project>/memory/`:
   - Update or create relevant memory files for anything learned this session
   - Update `MEMORY.md` index if new files were added
   - Save any user corrections as feedback memories

3. **Stage and commit** if there are uncommitted changes:
   - `git add` relevant files (not secrets, not data dirs)
   - Commit with a descriptive message
   - Push if a remote is configured

4. **Report** — short bullet summary of what was done and what's next.

If the project already has a project-specific `/handoff` command (e.g. in `.claude/commands/handoff.md` at the project level), that one takes precedence over this global version. This is the fallback for projects that don't have their own.
