# Cranelift no_std Issue — File on bytecodealliance/wasmtime

**Title:** Feature request: official no_std support for Cranelift

**Body:**

## Summary

We successfully ported Cranelift to run on a bare-metal `#![no_std]` OS ([ClaudioOS](https://github.com/suhteevah/claudio-os)) and achieved JIT compilation on bare metal — `add(3, 4) = 7` from Cranelift-generated x86_64 machine code with no OS, no POSIX, no libc.

This required forking **6 crates** and patching **200+ files** to remove `std` dependencies. We believe Cranelift is an ideal JIT backend for embedded/bare-metal/unikernel targets and would benefit from official `no_std` support.

## What we had to change

### 1. ISLE code generator emits `std::` paths
The ISLE-generated Rust code uses `std::vec::Vec`, `std::string::String`, `std::fmt`, etc. as fully-qualified paths. We had to add a build.rs post-processing step that replaces `std::` -> `core::`/`alloc::` in all generated `.rs` files.

**Suggested fix:** ISLE codegen should emit `core::`/`alloc::` paths, or have a configuration option for the path prefix.

### 2. `cranelift-codegen` timing module depends on `std`
`timing.rs` uses `std::time::Instant` and thread-local storage (`std::cell::RefCell` + `thread_local!`). We replaced the entire module with no-op stubs.

**Suggested fix:** Gate timing/profiling behind a `std` feature flag. The no-op path is fine for production use.

### 3. `HashMap`/`HashSet` from `std::collections`
Multiple modules use `std::collections::HashMap`. We substituted `hashbrown` (which is what `std` uses internally anyway).

**Suggested fix:** Use `hashbrown` directly or abstract behind a feature-gated type alias.

### 4. `gimli` dependency for unwind info
`gimli` pulls in `std`. We had to remove/stub unwind table generation entirely.

**Suggested fix:** Gate `gimli`/unwind support behind a feature flag (partially done via `unwind` feature but not fully decoupled).

### 5. `cranelift-frontend` has the same issues
Needed identical `std` -> `core`/`alloc` patching.

### 6. `cranelift-codegen-shared` and `cranelift-control` also use `std`
The entire crate graph needs `no_std` propagation.

## Proof of concept

Our implementation: https://github.com/suhteevah/claudio-os

Key crates:
- `crates/cranelift-codegen-nostd/` — forked cranelift-codegen with no_std patches
- `crates/cranelift-frontend-nostd/` — forked cranelift-frontend
- `crates/cranelift-codegen-shared-nostd/` — forked shared types
- `crates/cranelift-control-nostd/` — forked control crate
- `crates/rustc-lite/` — minimal Rust compiler using the above

Standalone: https://github.com/suhteevah/rustc-lite

## Environment

- Cranelift version: 0.116.1
- Target: `x86_64-unknown-none` (custom bare-metal target with SSE+AES-NI)
- No `std`, no `libc`, no POSIX — pure `core` + `alloc` with a 16 MiB heap

## Use cases for no_std Cranelift

- JIT compilation in unikernels and bare-metal OSes
- Embedded systems with dynamic code generation
- Hypervisor-level JIT (no guest OS needed)
- WebAssembly compilation on constrained targets
- Self-hosting compiler on bare metal

We'd be happy to contribute patches upstream if there's interest in official support.
