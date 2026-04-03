# r/selfhosted Post

**Subreddit**: r/selfhosted
**Title**: Self-hosted AI agents on bare metal -- no Docker, no VMs, just boot and go

---

**TLDR**: ClaudioOS is a bare-metal OS that boots on x86_64 hardware and runs multiple Claude (Anthropic) coding agents simultaneously. No Linux, no Docker, no VMs. Flash a USB drive, boot, authenticate, and you have a dedicated AI workstation with SSH access. Under 2 seconds to boot, under 32 MB RAM idle.

---

## What it is

ClaudioOS is a purpose-built operating system for running AI coding agents. You flash it to a USB drive or disk image, boot a machine from it, and you get:

- **Multiple Claude agent sessions** running in split-pane terminals, each with its own conversation state and tool access
- **Built-in tools** agents can use: text editor, Python interpreter, Rust compiler, web browser (text mode), shell with 45+ commands
- **SSH access** so you can connect from your laptop and interact with agents remotely
- **Networking from boot** -- DHCP, DNS, TLS 1.3, direct HTTPS to api.anthropic.com

No Linux underneath. No Docker. No Node.js. No container orchestration. The OS IS the application.

## Why self-host this way?

**1. Dedicated hardware utilization.** If you have a spare machine (old desktop, NUC, server), ClaudioOS turns it into a dedicated AI workstation. No OS overhead consuming resources that should go to your agents.

**2. Anthropic Max subscription.** The Max plan gives API access to Claude. ClaudioOS is designed to make that subscription maximally useful -- run multiple agents in parallel on hardware you own, each working on different tasks.

**3. Boot time.** Under 2 seconds from power-on to a functional shell with networking. No systemd, no service startup, no kernel module loading. You get to work immediately.

**4. Memory footprint.** Under 32 MB idle. A minimal Linux + Docker setup idles at 300+ MB before you run anything. On a 16 GB machine, that is 16 GB for your agents instead of 15.7 GB.

**5. Attack surface.** 222K lines of Rust vs. 30 million lines of C in the Linux kernel alone, plus Docker, plus whatever runtime your agents need. Rust's memory safety eliminates entire CVE classes.

## How it works

```
Power on -> UEFI boot -> ClaudioOS kernel
  -> Hardware init (2 seconds)
  -> DHCP + DNS (automatic)
  -> TLS 1.3 to api.anthropic.com
  -> Authenticate (API key baked in, or OAuth device flow)
  -> SSH daemon starts (post-quantum key exchange)
  -> Agent dashboard launches
  -> You start working
```

The dashboard is a tmux-style split-pane terminal. Ctrl+B is the prefix key. Create new agent panes, switch focus, resize. Each agent runs as an async task with up to 20 rounds of tool use per turn.

## SSH access

ClaudioOS includes a post-quantum SSH daemon (ML-KEM-768 + X25519 hybrid key exchange). Connect from any SSH client:

```
ssh user@<machine-ip>
```

You get a shell with 45+ builtins and a natural language mode -- type a question in English and it routes to Claude for interpretation.

## Hardware requirements

- **CPU**: x86_64 with UEFI boot and AES-NI (Intel Haswell or newer, most AMD from 2013+)
- **RAM**: 512 MB minimum, 4+ GB recommended for multiple agents
- **Network**: Ethernet (VirtIO in VMs, Intel I219/I225 on real hardware)
- **Storage**: USB drive or disk for the boot image

Tested on: QEMU, i9-11900K desktop, HP Victus laptop, Supermicro SYS-4028GR-TRT server.

## What agents can do

Each agent session has access to:

| Tool | What it does |
|------|-------------|
| edit_file | Nano-like text editor for creating/modifying files |
| execute_python | Run Python scripts (built-in interpreter, no CPython needed) |
| compile_rust | Compile Rust code via built-in Cranelift backend |
| web_browse | Text-mode web browser for looking up docs |
| shell | Run shell commands (45+ builtins: ls, cat, grep, find, etc.) |

Agents run a tool loop: send a message, Claude responds with tool calls, the OS executes them, sends results back, repeat (up to 20 rounds per turn).

## Current limitations (honest assessment)

- **No GPU passthrough yet.** The NVIDIA GPU driver is scaffolding. Agents cannot run local models.
- **WiFi is experimental.** Ethernet is reliable. WiFi (Intel AX201/AX200) is implemented but not battle-tested.
- **SSH crypto uses placeholder implementations.** The key exchange structure is post-quantum but the actual curve25519/Ed25519 operations are not wired to real crypto libraries yet. Do not expose port 22 to the internet.
- **No web UI.** This is a terminal-based system. If you want a browser-based dashboard, this is not the project for you (yet).
- **Single user.** No multi-tenancy, no access control between agents. This is designed for your personal hardware.

## Links

- **GitHub**: https://github.com/suhteevah/baremetal-claude
- **Website**: https://claudioos.vercel.app
- **Getting started**: https://github.com/suhteevah/baremetal-claude/blob/main/docs/building.md

Happy to answer questions about the setup, hardware compatibility, or anything else. If you have a spare machine gathering dust and an Anthropic API key, this might be a fun weekend project.
