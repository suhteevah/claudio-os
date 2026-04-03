# ClaudioOS Shell Documentation

## Overview

ClaudioOS includes an AI-native shell (`crates/shell/`, 2,884 lines) that combines
traditional Unix-like commands with natural language processing. Type `ls /mnt/nvme0`
or `"show me what's on the NVMe drive"` -- both work.

The shell now has 45+ built-in commands spanning filesystem operations, networking,
security, power management, and system administration.

The shell is `#![no_std]` and runs directly on the bare-metal kernel.

---

## Module Structure

| Module | Purpose |
|--------|---------|
| `shell.rs` | Main shell loop: line reading, command dispatch, history |
| `parser.rs` | Command parsing: tokenization, pipes, redirects, quoting |
| `builtin.rs` | 45+ built-in commands + VFS/SystemInfo trait abstractions |
| `pipe.rs` | Pipeline executor: connect commands with byte-stream pipes |
| `env.rs` | Environment variables: get, set, expand `$VAR` in arguments |
| `ai.rs` | AI mode: send natural language to Claude, execute returned commands |
| `prompt.rs` | Prompt rendering: username, cwd, colors |
| `script.rs` | Script runner: if/for/while control flow, variable expansion |

---

## Built-in Commands Reference

### Filesystem Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `ls` | `ls [path]` | List directory contents |
| `cd` | `cd <path>` | Change working directory |
| `pwd` | `pwd` | Print working directory |
| `cat` | `cat <file> [file...]` | Display file contents (accepts stdin via pipe) |
| `cp` | `cp <src> <dst>` | Copy a file |
| `mv` | `mv <src> <dst>` | Move or rename a file |
| `rm` | `rm <path>` | Remove a file or directory |
| `mkdir` | `mkdir <path>` | Create a directory |
| `touch` | `touch <path>` | Create an empty file or update timestamp |
| `head` | `head [-n N] <file>` | Show first N lines (default 10) |
| `tail` | `tail [-n N] <file>` | Show last N lines (default 10) |
| `grep` | `grep <pattern> [file]` | Search for pattern in file or stdin |
| `mount` | `mount <device> <path> <fstype>` | Mount a filesystem |
| `umount` | `umount <path>` | Unmount a filesystem |
| `df` | `df` | Show disk space usage per mount point |

### Process / Agent Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `ps` | `ps` | List active agent sessions (id, name, status) |
| `kill` | `kill <id>` | Kill an agent session by ID |

### System Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `clear` | `clear` | Clear the terminal screen |
| `reboot` | `reboot` | Reboot the system (via ACPI or keyboard controller) |
| `shutdown` | `shutdown` | Shutdown the system (via ACPI) |
| `date` | `date` | Show current date and time (from RTC wall clock) |
| `uptime` | `uptime` | Show system uptime |
| `free` | `free` | Show memory usage (total, used, free) |

### Network Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `ifconfig` | `ifconfig` | Show network interface info (name, IP, MAC, status) |
| `ping` | `ping <host>` | Ping a host with ICMP echo, show round-trip time |
| `wget` | `wget <url>` | Download a file from a URL via HTTP/HTTPS |
| `curl` | `curl <url>` | Fetch URL content and display to stdout |
| `netstat` | `netstat` | Show active network connections and listening ports |
| `dns` | `dns <hostname>` | Resolve a hostname to IP address |
| `nslookup` | `nslookup <hostname>` | DNS lookup with detailed resolver info |
| `traceroute` | `traceroute <host>` | Trace the route packets take to a host |
| `ssh` | `ssh <host>` | SSH client (placeholder) |

### Environment Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `set` | `set VAR=value` | Set an environment variable |
| `unset` | `unset VAR` | Remove an environment variable |
| `export` | `export VAR=value` | Set and export an environment variable |
| `echo` | `echo [args...]` | Print arguments to stdout |

### Meta Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `help` | `help` | Show list of available commands |
| `history` | `history` | Show command history |
| `exit` | `exit` | Exit the shell |

### Theme Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `theme` | `theme <name>` | Switch color theme at runtime |
| `theme list` | `theme list` | List all 9 available themes |

Available themes: `default`, `solarized-dark`, `solarized-light`, `monokai`,
`dracula`, `nord`, `gruvbox`, `claudioos`, `templeos`.

### Screensaver Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `screensaver` | `screensaver <mode>` | Activate a screensaver mode |
| `screensaver list` | `screensaver list` | List all 5 available modes |

Available modes: `starfield`, `matrix`, `bouncing`, `pipes`, `clock`.
The screensaver activates automatically after 5 minutes of idle time.
Any keypress deactivates it.

### Security Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `fw` | `fw <allow\|deny\|list\|flush>` | Firewall management: add/remove rules, list active rules |
| `fw allow <port>` | `fw allow 80` | Allow inbound traffic on a port |
| `fw deny <ip>` | `fw deny 10.0.0.5` | Block traffic from an IP address |
| `cryptsetup` | `cryptsetup <open\|close\|status>` | Disk encryption management (LUKS-compatible) |

### Power Management Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `battery` | `battery` | Show battery status, percentage, charging state |
| `suspend` | `suspend` | Suspend to RAM (ACPI S3) |

### System Administration Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `crontab` | `crontab <list\|add\|remove>` | Manage scheduled periodic tasks |
| `swapon` | `swapon <device>` | Enable swap on a device or partition |
| `man` | `man <command>` | Display manual page for a command |

### Conversation Management Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `conversations` / `convos` | `conversations` | List recent claude.ai conversations |
| `conv use <uuid>` | `conv use abc123...` | Switch agent to use this conversation |
| `conv rename <uuid> <name>` | `conv rename abc123 "My Project"` | Rename a conversation |
| `conv delete <uuid>` | `conv delete abc123...` | Delete a conversation |
| `conv new [name]` | `conv new "New Chat"` | Start a new conversation |

These commands require claude.ai auth mode. They interact with the claude.ai REST API
to manage conversations.

### IPC Commands

| Command | Usage | Description |
|---------|-------|-------------|
| `/msg <agent> <text>` | `/msg agent-1 hello` | Send a message to another agent |
| `/broadcast <text>` | `/broadcast attention` | Send a message to all agents |
| `/inbox` | `/inbox` | Read pending messages from other agents |
| `/agents` | `/agents` | List all registered agents with IPC |
| `/channel create <name>` | `/channel create shared-data` | Create a named data channel |
| `/channel read <name>` | `/channel read shared-data` | Read from a named channel |
| `/channel write <name> <data>` | `/channel write shared-data hello` | Write to a named channel |

---

## AI-Native Mode

When input does not match any built-in command, the shell sends it to Claude as
natural language. Claude interprets the request and returns executable commands.

### How It Works

1. User types: `show me the largest files on disk`
2. Shell detects this is not a built-in command
3. The `AiShellCallback` trait sends the text to the active Claude agent
4. Claude responds with commands: `ls -la / | sort -k5 -rn | head -20`
5. Shell executes the returned commands
6. Output is displayed to the user

### AiShellCallback Trait

```rust
pub trait AiShellCallback {
    /// Send natural language text to Claude, return suggested commands.
    fn query_ai(&mut self, input: &str) -> Result<String, String>;
}
```

The kernel provides the implementation that bridges to the active agent session.

---

## Pipes and Redirects

The shell supports Unix-style pipes connecting commands:

```
cat /etc/hosts | grep localhost | head -5
```

### Pipeline Execution

The `PipelineExecutor` connects commands by passing the stdout of one command
as the stdin of the next:

```
Command A (stdout) -> bytes -> Command B (stdin) -> bytes -> Command C (stdin)
```

Each built-in command accepts an optional `stdin: Option<&[u8]>` parameter for
receiving piped input.

### Redirects (Planned)

Output redirection (`>`, `>>`) and input redirection (`<`) are parsed but not
yet fully wired to the VFS.

---

## Environment Variables

The `Environment` struct manages shell variables:

```
set HOME=/home/claudio
set PATH=/bin:/usr/bin
echo $HOME         # prints /home/claudio
```

### Variable Expansion

Arguments containing `$VAR` are expanded before command execution. Supports:
- `$VAR` -- expand named variable
- `${VAR}` -- expand with explicit boundaries
- `$?` -- last command exit status

### Built-in Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `HOME` | `/` | Home directory |
| `CWD` | `/` | Current working directory (synced with `cd`) |
| `PS1` | `claudio$` | Shell prompt string |

---

## Shell Scripting

The `ScriptRunner` supports basic control flow:

### If/Else

```bash
if [ -f /etc/hostname ]; then
    cat /etc/hostname
else
    echo "no hostname"
fi
```

### For Loops

```bash
for f in /etc/*; do
    echo $f
done
```

### While Loops

```bash
while true; do
    date
done
```

---

## Tab Completion and History

### History

- Commands are stored in an in-memory history buffer
- Up/Down arrow keys navigate through previous commands
- `history` command displays the full history list

### Tab Completion (Planned)

Tab completion for file paths and command names is parsed but not yet wired
to the VFS for live path completion.

---

## Integration

The shell runs as a pane type in the agent dashboard. It is fully integrated
and accessible via `Ctrl+B s` (create new shell pane).

```rust
use claudio_shell::{Shell, LineReader, Environment};

let mut env = Environment::new();
let mut shell = Shell::new(env);
// shell.run(vfs, system_info, ai_callback);
```
