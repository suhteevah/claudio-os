# Wraith Browser — Bare Metal Port Specification

## What ClaudioOS Is

ClaudioOS is a **Rust `#![no_std]` bare-metal OS** that boots directly on x86_64 UEFI hardware (dev on QEMU, prod targets real machines). It has:

- **No Linux kernel, no POSIX, no libc**
- Custom async executor (interrupt-driven, cooperative)
- VirtIO-net NIC driver → smoltcp TCP/IP stack (DHCP + DNS working)
- GOP framebuffer with font rendering + split-pane terminal
- 16 MiB heap via linked_list_allocator
- PS/2 keyboard input via IRQ1

**Target use case**: Run multiple AI coding agents (Claude) simultaneously on dedicated hardware. Each agent gets a terminal pane. The OS talks directly to the Anthropic API. Wraith-browser integration enables OAuth authentication and web-based tool use for the agents.

## What We Need From Wraith

### Priority 1: Headless HTTP Client (for OAuth + API)

**UPDATE (2026-03-31):** TLS is now working via `embedded-tls` with a custom target
(`x86_64-claudio.json`) that enables SSE+AES-NI. The original blocker (LLVM crash)
was solved by adding `+sse,+sse2,+aes,+pclmulqdq` to the target features. QEMU
requires `-cpu Haswell` for the AES-NI instructions.

The HTTP/TLS stack is now complete. `wraith-transport` has been implemented to use
ClaudioOS's existing `claudio-net` TCP + TLS stack directly.

### Priority 2: DOM Parser for OAuth Pages

For browser-based OAuth (Anthropic console login), we need:
- HTML parsing (wraith already uses `scraper`/`html5ever`)
- Form detection + submission
- Cookie handling
- Redirect following

**Ask**: Make `sevro-headless`'s DOM parsing work in `no_std` + `alloc` context.

`scraper` depends on `html5ever` which depends on `std`. Need:
- `html5ever` compiled for `no_std` (or a lightweight alternative like `lol_html`)
- `scraper`'s CSS selector engine for `no_std`

### Priority 3: Framebuffer Rendering (Phase 5)

For visual OAuth in a terminal pane, we'd need:
- Text-based rendering of web pages (like `links`/`lynx` browser)
- Render HTML → character cells that we draw to our GOP framebuffer
- No GPU, no compositor — just character-cell output

This is the "wraith in a pane" vision.

---

## Technical Constraints

### What's Available on ClaudioOS

| Feature | Status |
|---------|--------|
| Heap allocation (`alloc`) | Yes, 16 MiB |
| TCP/IP (smoltcp) | Working (DHCP, DNS, TCP connect) |
| TLS 1.3 (embedded-tls) | Working (AES-128-GCM-SHA256, requires -cpu Haswell) |
| HTTP/1.1 + SSE | Working (chunked encoding, streaming) |
| Anthropic Messages API | Working (SSE streaming, tool use) |
| Async runtime | Custom executor (not tokio) |
| File I/O | FAT32 stub (not yet wired) |
| Threads | None — single-core cooperative async |
| `std` library | **NOT AVAILABLE** |
| libc / POSIX | **NOT AVAILABLE** |
| Dynamic linking | **NOT AVAILABLE** |
| FPU/SSE/AES-NI | Enabled via custom target (x86_64-claudio.json) |

### Crates That Won't Compile for `x86_64-unknown-none`

| Crate | Reason | Alternative |
|-------|--------|-------------|
| `tokio` | Needs `std`, threads, epoll/kqueue | ClaudioOS async executor |
| `reqwest` | Needs tokio + native-tls/rustls | Raw HTTP over smoltcp |
| `rustls` | Needs `ring` or `aws-lc-rs` (both need `std`) | `embedded-tls` with soft crypto |
| `rquest` | Fork of reqwest, same deps | Raw HTTP |
| `std::net` | No `std` | smoltcp sockets |
| `std::fs` | No `std` | FAT32 via `fatfs` crate |
| `std::time` | No `std` | PIT timer counter (18.2 Hz) |
| `rquickjs` | Needs C compiler + linking | Defer JS to Phase 5 |

### What CAN Compile

| Crate | Status |
|-------|--------|
| `serde` + `serde_json` | Working (`no_std` + `alloc`) |
| `smoltcp` | Working |
| `vte` (ANSI parser) | Working |
| `html5ever` | Needs investigation — may need `std` features disabled |
| `scraper` | Depends on html5ever |
| `log` | Working |
| `spin` (mutex) | Working |

---

## Bugs / Issues in Current Wraith That Affect the Port

### Bug 1: `sevro-headless` Depends on `reqwest` Directly
`sevro/ports/headless/Cargo.toml` lists `reqwest` as a hard dependency. For bare metal, HTTP fetching needs to go through our smoltcp stack. Need a trait abstraction for the HTTP transport layer.

### Bug 2: `rquickjs` is a Hard Dependency
The JS runtime (`rquickjs`) is linked unconditionally in sevro-headless. For bare metal Phase 1, we need DOM parsing WITHOUT JS execution. Make JS optional behind a feature flag.

### Bug 3: No `no_std` Feature Flags
None of the wraith crates have `#![no_std]` support or feature flags to disable std-dependent functionality. Need:
```toml
[features]
default = ["std"]
std = ["reqwest", "tokio", "rquickjs"]
no_std = []  # Uses alloc only, no tokio/reqwest/JS
```

### Bug 4: `browser-core` Assumes Tokio Runtime
`BrowserEngine` trait methods are `async` and assume tokio. For bare metal, they need to work with any async executor. Consider making the trait runtime-agnostic.

---

## Feature Requests

### FR 1: `HttpTransport` Trait
Abstract the HTTP layer so different backends can be plugged in:
```rust
pub trait HttpTransport {
    async fn request(&self, req: HttpRequest) -> Result<HttpResponse, Error>;
}
```
- `std` impl: uses reqwest (current behavior)
- `no_std` impl: uses smoltcp raw TCP + manual HTTP/1.1

### FR 2: `no_std` DOM Parser
Extract the DOM parsing pipeline into a standalone `no_std` crate:
- HTML → DOM tree (html5ever or alternative)
- CSS selector queries
- Element attribute access
- No JS, no network, just parsing

### FR 3: Text-Mode Page Renderer
Render a parsed DOM tree into a character-cell grid:
```rust
pub fn render_to_text(dom: &Dom, width: usize, height: usize) -> Vec<Vec<char>>
```
- Handles basic block/inline layout
- Links shown as `[text](url)` or highlighted
- Forms shown with `[input]` `[submit]` markers
- Tables as ASCII tables

### FR 4: Feature-Gated Build
```toml
[features]
default = ["std-full"]
std-full = ["tokio", "reqwest", "rquickjs", "rustls"]
std-headless = ["tokio", "reqwest"]  # No JS
no-std-http = ["alloc"]              # Just HTTP transport trait
no-std-dom = ["alloc", "html5ever"]  # HTTP + DOM parsing
no-std-render = ["no-std-dom"]       # + text rendering
```

---

## Architecture for the Port

```
ClaudioOS Kernel
    │
    ├── wraith-transport (no_std)
    │   └── HttpTransport impl over smoltcp
    │
    ├── wraith-dom (no_std)
    │   ├── html5ever (no_std fork or lol_html)
    │   └── CSS selector engine
    │
    ├── wraith-render (no_std)
    │   └── DOM → character cells → framebuffer pane
    │
    └── wraith-browser-core (existing, std)
        └── Full browser with JS, tokio, etc.
            (runs on Linux/macOS/Windows as before)
```

The `no_std` crates are NEW, extracted from wraith's existing code but stripped of std dependencies. The existing wraith codebase continues to work unchanged on normal OS targets.

---

## Timeline Estimate

| Phase | Work | Effort |
|-------|------|--------|
| Port HTTP transport | New trait + smoltcp impl | 2-3 days |
| Fix TLS for bare metal | Software-only crypto, no SIMD | 3-5 days |
| Port DOM parser | html5ever no_std or alternative | 3-5 days |
| Text renderer | DOM → character grid | 2-3 days |
| Integration | Wire into ClaudioOS kernel | 1-2 days |
| **Total** | | **~2-3 weeks** |

---

## Contact

- **ClaudioOS repo**: https://github.com/suhteevah/baremetal-claude
- **Owner**: Matt Gates (suhteevah) — Ridge Cell Repair LLC
- **Wraith repo**: https://github.com/suhteevah/wraith-browser
