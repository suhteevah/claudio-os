# Claude Code Usage Notes for ClaudioOS

Reference for the underused features wired into this project.

## Hooks (already configured in `.claude/settings.local.json`)

| Hook | What it does |
|------|--------------|
| `TaskCompleted` | Runs `cargo check --workspace` when Rust files were modified. Exit 2 blocks completion. |
| `CwdChanged` | Logs every `cd` to `.claude/cwd-history.log` for observability. |
| `FileChanged` (Cargo.toml/Cargo.lock) | Runs background `cargo check` to warm the build cache. |

To check the logs:
```bash
cat .claude/cwd-history.log
cat .claude/cargo-warmup.log
```

## /loop — schedule recurring commands

Useful patterns for ClaudioOS:

```text
/loop 15m cargo check --workspace and report any new warnings
```
Continuously monitors build health while you work on something else.

```text
/loop 30m run all tests in the rustc-lite crate and report failures
```
Background test sentinel.

```text
/loop 1h /review-pr if any PRs are open
```
Periodic PR review automation.

Each iteration shows up as a normal turn — jump in whenever you want to redirect.

## Channels (Telegram/Discord/iMessage)

See `CHANNELS_SETUP.md` for configuration. Once set up, run:
```bash
claude --channels
```
And Claude can receive DMs that get pushed into the current session.

## Subagent persistent memory

If we create custom project-specific subagents in `.claude/agents/`, add this
to the frontmatter so they remember across sessions:
```yaml
---
name: my-rust-reviewer
description: Reviews ClaudioOS Rust code for no_std issues
memory: user   # writes to ~/.claude/agent-memory/my-rust-reviewer/
---
```

## CLAUDE_CODE_NEW_INIT=1

If we ever regenerate `CLAUDE.md`, set this env var first:
```bash
CLAUDE_CODE_NEW_INIT=1 claude
```
Then run `/init` — Claude will interview the codebase and ask questions before
generating the file, much better than the default one-shot approach.

## HTML comments in CLAUDE.md

Block-level `<!-- -->` HTML comments in `CLAUDE.md` are stripped before context
injection. Use them for maintainer notes that don't need to cost tokens:

```markdown
<!--
Last reviewed: 2026-04-07
History of phases moved here to save context tokens.
Phase 1: Boot to terminal (complete)
Phase 2: Networking + TLS (complete)
...
-->
```

The phase history in this repo's `CLAUDE.md` is wrapped this way.

## Agent teams

For complex tasks, create a team via `/agents` or natural language:
```text
Create a team: one on architecture, one on implementation, one as devil's advocate.
Have them produce a plan for X before writing any code.
```

Teammates message each other and share task lists, unlike subagents (which only
report back to you).
