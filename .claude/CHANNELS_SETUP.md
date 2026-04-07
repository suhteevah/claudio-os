# Channels Setup — Telegram (Discord/iMessage available too)

Claude Code can receive DMs from Telegram, Discord, or iMessage and push them
into a running session with full context. Use this for long jobs (training,
builds) so you get pinged when something needs attention.

## One-time setup

1. **Make sure you're logged in via claude.ai** (not API key — Channels needs the OAuth login)
   ```bash
   claude --login
   ```

2. **Install the Telegram channel plugin**
   ```bash
   claude --channels plugin:telegram@claude-plugins-official
   ```
   This walks through:
   - Creating a Telegram bot via @BotFather
   - Linking it to your claude.ai account
   - Setting up DM permissions

3. **Verify it's running**
   ```bash
   claude --channels status
   ```

## Using it in a session

Once configured, just start Claude with the flag:
```bash
claude --channels
```

Now any DM you send to your bot on Telegram gets pushed into the *current*
Claude session. Claude has full context — it can see what you're working on
and respond accordingly.

## Use cases for ClaudioOS

- **Long QLoRA training runs**: Claude sees the training process complete (or fail),
  diagnoses the failure, and DMs you a summary on Telegram.
  Reply "do it" to apply the fix.

- **Cargo build at 2am**: CI fails overnight, Claude diagnoses and proposes fix
  via Telegram before you wake up.

- **Network/hardware events on physical ClaudioOS hardware**: when running on the
  i9-11900K box, kernel events can be forwarded as Telegram pings.

## Notes
- Channels requires claude.ai login, NOT an API key
- Events arrive in the *currently open session*, not as new sessions
- Telegram, Discord, iMessage all supported via the official plugin
