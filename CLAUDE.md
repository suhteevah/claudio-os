# CLAUDE.md — ClaudioOS Build Instructions

## What This Is

ClaudioOS is a bare-metal Rust operating system that boots via UEFI and provides a
purpose-built environment for running multiple AI coding agents (Anthropic Claude)
simultaneously. It has NO Linux kernel, NO POSIX layer, NO JavaScript runtime. It is
a single-address-space async Rust application that manages its own hardware.

**Owner**: Matt Gates (suhteevah) — Ridge Cell Repair LLC
**Target hardware**: x86_64 UEFI machines (dev on QEMU, prod on production hardware,
dedicated servers, laptops)

## Handoff Protocol
- ALWAYS read HANDOFF.md and recent memory/vault notes BEFORE diagnosing any issue
- Trust prior session findings - do not re-diagnose bugs already patched
- Update HANDOFF.md at end of session with current state, blockers, and next steps

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                  Agent Dashboard                     │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐            │
│  │ Agent 1  │ │ Agent 2  │ │ Agent 3  │  ...       │
│  │ (pane)   │ │ (pane)   │ │ (pane)   │            │
│  └──────────┘ └──────────┘ └──────────┘            │
├─────────────────────────────────────────────────────┤
│              Agent Session Manager                   │
│         (async tasks, one per agent)                 │
├──────────────┬──────────────┬───────────────────────┤
│  API Client  │     Auth     │   Terminal Renderer   │
│  (Messages   │  (OAuth      │   (framebuffer +      │
│   API + SSE) │   device     │    ANSI + split       │
│              │   flow)      │    panes)             │
├──────────────┴──────────────┴───────────────────────┤
│              Async Executor (interrupt-driven)        │
├─────────────────────────────────────────────────────┤
│     Net (smoltcp + TLS)    │    FS (FAT32 persist)  │
├────────────────────────────┴────────────────────────┤
│   NIC Driver (virtio-net / e1000)  │  PS/2 Keyboard │
├────────────────────────────────────┴────────────────┤
│              x86_64 Kernel Core                      │
│   (paging, heap, GDT/IDT, interrupts, PCI)          │
├─────────────────────────────────────────────────────┤
│              UEFI Boot (bootloader crate)             │
└─────────────────────────────────────────────────────┘
```

## Crate Structure

- **`kernel/`** — Binary entry point. Boots, inits hardware, starts async executor,
  launches agent dashboard. This is `#![no_std]` + `#![no_main]`.
- **`crates/api-client/`** — Anthropic Messages API client. Pure `no_std` + `alloc`.
  Handles streaming SSE, tool use protocol, conversation state. NO reqwest, NO hyper.
  Raw HTTP/1.1 over a TLS byte stream.
- **`crates/auth/`** — OAuth 2.0 Device Authorization Grant (RFC 8628). Token persist
  to FAT32, background refresh task, credential store shared across all agents.
- **`crates/terminal/`** — Framebuffer terminal renderer with split-pane support.
  Uses `vte` + `noto-sans-mono-bitmap`. Each pane is a viewport into the GOP
  framebuffer with independent scroll state.
- **`crates/net/`** — VirtIO-net driver, smoltcp integration, DHCP, DNS, TLS 1.3
  wrapper. Provides `TlsStream` type that the API client consumes.
- **`crates/agent/`** — Agent session lifecycle. Each session is an async task with
  its own conversation history, tool execution (max 20 rounds), and terminal pane.
- **`crates/fs-persist/`** — FAT32 persistence layer (stubbed). Config, tokens,
  agent state, conversation logs.
- **`crates/editor/`** — Nano-like text editor (~400 lines, 11 tests). Provides
  `edit_file` tool for Claude agents.
- **`crates/python-lite/`** — Minimal Python interpreter (tokenizer, parser, eval).
  Variables, loops, functions, 28 tests. Provides `execute_python` tool.
- **`crates/rustc-lite/`** — Bare-metal Rust compiler using Cranelift backend.
- **`crates/cranelift-*-nostd/`** — Forked Cranelift crates (codegen, frontend,
  codegen-shared, control) patched for `#![no_std]`.
- **`crates/rustc-hash-nostd/`** — Forked rustc-hash for `no_std`.
- **`crates/arbitrary-stub/`** — Stub implementation of the arbitrary crate for
  `no_std` (Cranelift dependency).
- **`crates/wraith-dom/`** — `no_std` HTML parser, CSS selectors, form detection
  (2,070 lines, 32 tests).
- **`crates/wraith-transport/`** — HTTP/HTTPS over smoltcp (572 lines).
- **`crates/wraith-render/`** — HTML to text-mode character grid (1,225 lines,
  12 tests).
- **`crates/elf-loader/`** — ELF64 binary loader: parsing, relocation, execution
  (1,213 lines).
- **`crates/linux-compat/`** — Linux syscall translation layer for binary compat
  (4,090 lines). /proc emulation, signal dispatch, mmap stubs.

## Build & Run

### Prerequisites
```bash
rustup target add x86_64-unknown-none
cargo install bootimage  # or use bootloader's disk image builder
# QEMU for testing:
sudo apt install qemu-system-x86 ovmf
```

### Development cycle
```bash
# Build the kernel
cargo build

# Create bootable disk images (bootloader crate v0.11)
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os

# Run in QEMU with UEFI + networking + TLS support
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0,hostfwd=tcp::5555-:5555 \
    -serial stdio \
    -m 512M \
    -smp 4 \
    -cpu Haswell
```

**Note:** `-cpu Haswell` is required for AES-NI instructions used by TLS 1.3.

### QEMU networking note
`-netdev user` provides SLIRP NAT. The guest gets DHCP (10.0.2.x), DNS (10.0.2.3),
and outbound internet including HTTPS to api.anthropic.com. No bridging needed for dev.

## Critical Design Decisions

1. **NO JavaScript runtime.** We call the Anthropic API directly from Rust via
   HTTP/1.1 POST + SSE streaming. This eliminates 50+ syscalls worth of compat work.

2. **Single address space.** No kernel/user boundary, no syscalls, no process
   isolation. Every agent session is an async task. We trust our own code.

3. **OAuth device flow at boot.** The auth module gates agent session startup.
   Token persists to FAT32. Background refresh runs as an executor task.

4. **Interrupt-driven async executor.** Hardware interrupts (NIC rx, keyboard,
   timer) wake futures. No polling. `hlt` when idle for power savings.

5. **Split-pane terminal natively.** No tmux dependency. The terminal crate manages
   a layout tree of viewports over the GOP framebuffer.

6. **FAT32 only for persistence.** No ext4, no journaling. Simple, works, the
   `fatfs` crate handles it. Config + tokens + logs.

<!--
Development phase history is wrapped in HTML comments — Claude Code strips
block-level comments before injecting CLAUDE.md into context, so this content
is visible to humans reading the file but doesn't cost session tokens.
See .claude/USAGE_NOTES.md for the pattern.

## Development Phases

### Phase 1: Boot to terminal -- COMPLETE
- [x] Kernel boots via bootloader crate on QEMU with UEFI
- [x] GOP framebuffer initialized, can draw pixels
- [x] Serial debug output working (0x3F8)
- [x] GDT + IDT + interrupts configured (data segment fix, APIC disable)
- [x] Heap allocator working (linked_list_allocator, 16 MiB)
- [x] PS/2 keyboard input via IRQ1 (async ScancodeStream)
- [x] Basic terminal: type characters, see them on screen
- [x] ANSI escape sequence support via vte
- [x] SSE/SSE2/AVX enabled (CR0/CR4/XCR0 + CPUID detection)
- [x] 4 MiB heap-allocated kernel stack (bootloader stack exhaustion fix)
- [x] PIT timer at 18.2 Hz for timestamps
- [x] PCI bus enumeration with bus mastering

### Phase 2: Networking + TLS -- COMPLETE
- [x] VirtIO-net driver initialized via PCI enumeration (legacy 0.9.5)
- [x] smoltcp interface with DHCP obtaining IP + DNS
- [x] TCP connection to a known IP (test with httpbin.org)
- [x] DNS resolution working (resolve api.anthropic.com)
- [x] TLS 1.3 handshake (embedded-tls, AES-128-GCM-SHA256)
- [x] HTTPS GET to verify connectivity
- [x] HTTPS POST with JSON body
- [x] Chunked transfer encoding + SSE parsing
- [x] TCP send queue drain + CloseWait EOF detection
- [x] Nagle disabled for immediate packet transmission
- [x] Custom target x86_64-claudio.json with SSE+AES-NI (requires -cpu Haswell)

### Phase 3: API client + Auth -- COMPLETE
- [x] Auth relay (tools/auth-relay.py) for API key management
- [x] Compile-time CLAUDIO_API_KEY fallback
- [x] Anthropic Messages API: send prompt, receive response
- [x] SSE streaming: parse `event: content_block_delta` etc. (token-by-token)
- [x] Tool use protocol: parse tool_use blocks, return tool_result
- [x] Conversation state management
- [ ] OAuth device flow: display code, poll for token (deferred -- using auth relay)
- [ ] Token persistence to FAT32 image (deferred -- fs-persist stubbed)

### Phase 4: Multi-agent dashboard -- COMPLETE
- [x] Split-pane layout tree (horizontal/vertical splits)
- [x] Per-pane terminal instances with independent scroll
- [x] Keyboard shortcuts: Ctrl+B prefix (tmux-style) for pane mgmt
- [x] Agent session creation/destruction
- [x] Focus switching between panes
- [x] Agent tool loop (send -> tool_use -> execute -> resend, max 20 rounds)
- [x] Welcome banner rendering
- [ ] Status bar: agent states, token usage, network status (not yet wired)

### Phase 5: Development environment -- COMPLETE
- [x] python-lite: Minimal Python interpreter (vars, loops, functions, 28 tests)
- [x] Nano-like text editor (crates/editor, ~400 lines, 11 tests)
- [x] Rust build server (tools/build-server.py) + compile_rust tool
- [x] execute_python tool for Claude agents
- [x] Tools integrated into agent tool loop

### Phase 6: Self-hosting foundation -- COMPLETE
- [x] Cranelift code generator compiles for bare metal
- [x] Forked 6 crates for no_std (cranelift-codegen, cranelift-frontend,
      cranelift-codegen-shared, cranelift-control, rustc-hash, arbitrary)
- [x] libm for f32/f64 math in no_std
- [x] Build script post-processing for generated code std->core replacement
- [x] hashbrown with ahash for HashMap/HashSet
- [x] crates/rustc-lite: Bare-metal Rust compilation via Cranelift

### Phase 7: Wraith browser integration -- COMPLETE
- [x] wraith-dom: no_std HTML parser + CSS selectors + form detection (2,070 lines)
- [x] wraith-transport: HTTP/HTTPS over smoltcp (572 lines)
- [x] wraith-render: HTML -> text-mode character grid (1,225 lines)
- [ ] Wire into kernel boot for OAuth page rendering
- [ ] Form interaction (keyboard input at rendered form fields)

### Phase 8: Linux compatibility + advanced features -- COMPLETE
- [x] ELF loader: parse, relocate, execute ELF64 binaries (1,213 lines)
- [x] Linux syscall translation layer (4,090 lines crate + 301 lines kernel)
- [x] In-kernel vector database: cosine similarity, KNN search (1,062 lines)
- [x] Persistent agent memory: embeddings, semantic search (1,849 lines)
- [x] SSE streaming: backpressure, buffered rendering, rate limiting (280 lines)
- [x] Runtime model selection: Opus, Sonnet, Haiku (255 lines)
- [x] NTP time sync: network time protocol client (383 lines)
- [x] Native git client: clone, commit, push, pull, diff, log (2,120 lines)
- [x] Email client: SMTP send, IMAP receive, MIME parsing (967 lines)
- [x] Full-text search across files, conversations, agent output (494 lines)
- [x] System-wide notifications: priority levels, agent alerts (300 lines)
- [x] Image viewer: in-terminal rendering with dithering (413 lines)

### Future: Real hardware + hardening
- [ ] Boot on physical hardware (test on Arch box first)
- [ ] Wire VFS to real storage drivers (AHCI/NVMe + ext4/btrfs)
- [ ] Wire SSH shell to real shell crate
- [ ] GPU LLM inference (run local models on GPU)
- [ ] Graceful shutdown / token revocation
- [ ] USB boot image generation
-->

## Key Crate Versions & Docs

| Crate | Version | Docs |
|-------|---------|------|
| bootloader | 0.11 | https://docs.rs/bootloader/0.11 |
| bootloader_api | 0.11 | https://docs.rs/bootloader_api/0.11 |
| x86_64 | 0.15 | https://docs.rs/x86_64/0.15 |
| smoltcp | 0.12 | https://docs.rs/smoltcp/0.12 |
| vte | 0.15 | https://docs.rs/vte/0.15 |
| pc-keyboard | 0.8 | https://docs.rs/pc-keyboard/0.8 |
| fatfs | 0.4 | https://docs.rs/fatfs/0.4 |
| embedded-tls | 0.17 | https://docs.rs/embedded-tls/0.17 |
| linked_list_allocator | 0.10 | https://docs.rs/linked_list_allocator/0.10 |
| spin | 0.9 | https://docs.rs/spin/0.9 |
| serde_json (no_std) | 1.x | features = ["alloc"], default-features = false |

<!--
## Reference Projects

- **blog_os**: https://os.phil-opp.com — THE tutorial. Follow for kernel basics.
- **MOROS**: https://moros.cc — Hobby Rust OS with smoltcp networking. Great driver ref.
- **Motor OS**: https://motor-os.org — Rust microkernel that serves its own website.
- **Redox OS drivers**: https://gitlab.redox-os.org/redox-os/drivers — NIC/NVMe/USB ref.
- **Hermit OS**: https://hermit-os.org — Unikernel with smoltcp, good kernel structure.
- **os-terminal**: https://lib.rs/crates/os-terminal — Turnkey bare-metal terminal.
-->

## Conventions

- All crates are `#![no_std]` with `extern crate alloc` where heap is needed
- Use `log` crate macros everywhere, kernel provides serial + framebuffer log sinks
- Async where possible, `spin::Mutex` for shared state (no std Mutex available)
- Verbose logging: every network event, every API call, every auth state change
- Test in QEMU first, always. `cargo test` runs host-side unit tests for pure logic.
- Kernel panics print a red backtrace to framebuffer + serial before halting

## Debugging

### Diagnosis Discipline
- Investigate the actual error before suggesting reinstalls or generic fixes
- For Windows spawn/ENOENT errors: check shim file extensions (.exe vs shell scripts) FIRST
- Do not assume a dependency is missing until you've verified with `where`/`which`

## Tooling Preferences
- DO NOT use Playwright unless explicitly requested or no alternative exists
- Prefer Wraith browser MCP for scraping/browser automation
- For Unraid deployments: use plain `docker run`, not docker-compose (busybox shell limitation)

## Output Discipline
- Keep responses concise; avoid long explanations after task completion
- When hitting token limits, summarize and offer to continue rather than retrying full output
- Write verbose logs, detailed analysis, and long output to `scratch/` files instead of chat
- Chat responses should be short blurbs; user reviews `scratch/` logs when they want detail
- Checkpoint progress to HANDOFF.md every 10 tool calls during long autonomous runs

## Environment Variables (build-time)

- `CLAUDIO_API_KEY` — Optional baked-in API key for development (skips OAuth)
- `CLAUDIO_LOG_LEVEL` — trace/debug/info/warn/error (default: info)
- `CLAUDIO_QEMU` — Set to 1 to use QEMU-friendly defaults (VirtIO, SLIRP)
