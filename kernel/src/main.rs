//! ClaudioOS — Bare-metal Rust OS for AI coding agents
//!
//! Boot sequence:
//!   UEFI -> bootloader -> kernel_main -> init_hardware -> auth_gate -> agent_dashboard
//!
//! This is a single-address-space async application. No kernel/user boundary,
//! no syscalls, no process isolation. Every agent session is an async task.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod agent_loop;
mod dashboard;
mod executor;
mod framebuffer;
mod gdt;
mod interrupts;
mod keyboard;
mod logger;
mod memory;
mod pci;
mod serial;
mod terminal;

use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

/// Physical memory offset provided by the bootloader, stored globally so that
/// subsystems initialised after boot (e.g. networking) can translate addresses.
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Bootloader configuration — request a framebuffer and physical memory mapping
static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(bootloader_api::config::Mapping::Dynamic);
    // Request a larger kernel stack — default is too small for interrupt handlers
    // with the log crate's formatting. 128 KiB should be plenty.
    config.kernel_stack_size = 128 * 1024;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Primary kernel entry point — called by the bootloader after UEFI handoff.
///
/// At this point we have:
/// - Identity-mapped kernel code/data
/// - Physical memory offset mapping
/// - A GOP framebuffer
/// - A memory map from UEFI
/// - Interrupts disabled
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // ── Phase -1: Bare minimum proof-of-life (VGA text mode + raw serial) ──
    // Write directly to serial port 0x3F8 WITHOUT full UART init to prove
    // we actually reached kernel_main. VGA text buffer at 0xB8000 too.
    unsafe {
        // Raw serial write — just push bytes, QEMU's serial works even without full init
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"[claudio] kernel_main entered\r\n" {
            port.write(b);
        }
    }

    // ── Phase 0: Enable SSE/SSE2 — required for crypto crates (AES-NI, SHA) ──
    // The bootloader may or may not enable SSE. We ensure it's on.
    unsafe {
        // CR0: clear EM (bit 2), set MP (bit 1) — enable FPU/SSE
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // clear EM
        cr0 |= 1 << 1;    // set MP
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        // CR4: set OSFXSR (bit 9) + OSXMMEXCPT (bit 10) — enable SSE instructions
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= (1 << 9) | (1 << 10);
        core::arch::asm!("mov cr4, {}", in(reg) cr4);
    }

    // ── Phase 0a: Serial debug output (available immediately) ─────────
    serial::init();

    // ── Phase 0b: Logger (so all subsequent log::* calls produce output) ──
    logger::init();
    log::info!("[boot] ClaudioOS v{} starting", env!("CARGO_PKG_VERSION"));
    log::info!("[boot] SSE/SSE2 enabled");
    log::info!("[boot] bootloader handed off control");

    // ── Phase 1: CPU structures ──────────────────────────────────────
    gdt::init();
    log::info!("[boot] GDT initialized with TSS");

    // ── Phase 2: Memory management ───────────────────────────────────
    // Must initialize heap BEFORE enabling interrupts, because the IDT
    // lazy-init and interrupt handlers may allocate.
    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader must map physical memory");

    // Store phys_mem_offset globally so subsystems like networking can use it.
    PHYS_MEM_OFFSET.store(phys_mem_offset, Ordering::Relaxed);

    let memory_map = &boot_info.memory_regions;
    memory::init(phys_mem_offset, memory_map);
    log::info!("[boot] heap allocator initialized");

    // ── Phase 3: Interrupts (needs heap for keyboard queue allocs) ────
    interrupts::init();
    log::info!("[boot] IDT loaded, PIC initialized (interrupts still disabled)");

    // ── Phase 3b: Keyboard decoder ────────────────────────────────────
    keyboard::init();

    // ── Phase 4: Framebuffer ─────────────────────────────────────────
    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        log::info!(
            "[boot] framebuffer: {}x{} stride={} bpp={:?}",
            info.width,
            info.height,
            info.stride,
            info.pixel_format,
        );
        log::info!("[boot] clearing framebuffer...");
        framebuffer::init(fb, phys_mem_offset);
        log::info!("[boot] framebuffer initialized");
    } else {
        log::warn!("[boot] no framebuffer available, serial-only mode");
    }

    // ── Phase 5: PCI enumeration + device discovery ──────────────────
    log::info!("[boot] starting PCI enumeration...");
    pci::enumerate();
    log::info!("[boot] PCI enumeration complete");

    // ── Phase 6: Enable interrupts + async executor ──────────────────
    // The bootloader's kernel stack is nearly exhausted after all the init
    // (log formatting is very stack-heavy). Allocate a fresh 256 KiB stack
    // on the heap and switch to it before enabling interrupts.
    log::info!("[boot] allocating new kernel stack on heap...");
    const NEW_STACK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB — TLS handshake + crypto is very stack-heavy
    // Allocate with 16-byte alignment — required for FXSAVE in interrupt handlers
    let layout = alloc::alloc::Layout::from_size_align(NEW_STACK_SIZE, 16).unwrap();
    let new_stack_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if new_stack_ptr.is_null() { panic!("failed to allocate kernel stack"); }
    // Align stack top to 16 bytes (stack grows down, RSP must be 16-byte aligned)
    let new_stack_top = (new_stack_ptr as u64 + NEW_STACK_SIZE as u64) & !0xF;
    // Stack is raw-allocated — no need to forget, it lives forever.
    log::info!("[boot] new stack top: {:#x}", new_stack_top);

    // Switch to the new stack and continue execution there.
    // We pass the entry function pointer and new stack pointer to asm.
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "call {entry}",
            stack = in(reg) new_stack_top,
            entry = in(reg) post_stack_switch as *const (),
            options(noreturn)
        );
    }
}

/// Continuation after switching to the heap-allocated stack.
/// This function runs with a fresh 256 KiB stack — plenty of room for
/// interrupt handlers + executor + log formatting.
fn post_stack_switch() -> ! {
    log::info!("[boot] running on new stack!");
    log::info!("[boot] enabling interrupts and starting async executor");
    interrupts::enable();
    executor::run(async {
        main_async().await;
    });
    log::error!("[boot] executor returned unexpectedly");
    halt_loop()
}

/// Return the current time as a smoltcp Instant, derived from the PIT tick
/// counter in the timer interrupt handler.
fn now() -> claudio_net::Instant {
    claudio_net::Instant::from_millis(interrupts::millis_since_boot())
}

/// Decode complete HTTP chunks from `chunk_buf`, feed the decoded payload
/// bytes into the SSE `StreamParser`, and process resulting events through
/// the `StreamAccumulator`.  For each `TextDelta` event the text is printed
/// immediately to serial so the user sees tokens as they arrive.
///
/// Any incomplete chunk data is left in `chunk_buf` for the next call.
/// Sets `stream_done = true` when a `MessageStop` event is received.
fn decode_and_feed_chunks(
    chunk_buf: &mut alloc::vec::Vec<u8>,
    parser: &mut claudio_api::streaming::StreamParser,
    accumulator: &mut claudio_api::streaming::StreamAccumulator,
    stream_done: &mut bool,
) {
    use claudio_api::streaming::{StreamEvent, Delta};

    // Process all complete HTTP chunks in the buffer.
    // Chunk format: <hex-size>\r\n<data>\r\n
    loop {
        // Find the chunk size line ending (\r\n)
        let crlf_pos = match chunk_buf.windows(2).position(|w| w == b"\r\n") {
            Some(pos) => pos,
            None => break, // Need more data
        };

        // Parse the hex chunk size
        let size_str = match core::str::from_utf8(&chunk_buf[..crlf_pos]) {
            Ok(s) => s.trim(),
            Err(_) => break,
        };
        // Chunk size may have extensions after `;` — ignore them
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let chunk_size = match usize::from_str_radix(size_hex, 16) {
            Ok(s) => s,
            Err(_) => break,
        };

        // Terminal chunk (size 0) means end of chunked stream
        if chunk_size == 0 {
            log::debug!("[stream] received terminal chunk");
            break;
        }

        // Check if we have the full chunk: size_line + \r\n + data + \r\n
        let total_needed = crlf_pos + 2 + chunk_size + 2;
        if chunk_buf.len() < total_needed {
            break; // Need more data
        }

        // Extract the chunk payload
        let payload_start = crlf_pos + 2;
        let payload = chunk_buf[payload_start..payload_start + chunk_size].to_vec();

        // Consume this chunk from the buffer
        *chunk_buf = chunk_buf[total_needed..].to_vec();

        // Feed the decoded payload into the SSE stream parser
        let events = parser.feed(&payload);
        for event in &events {
            accumulator.process(event);
            match event {
                StreamEvent::MessageStart { message_id, model } => {
                    log::info!("[stream] message started: id={} model={}", message_id, model);
                }
                StreamEvent::ContentBlockDelta { delta: Delta::TextDelta { text }, .. } => {
                    // Print each text token immediately to serial — this is the
                    // key streaming UX: the user sees tokens as they arrive.
                    crate::serial_print!("{}", text);
                }
                StreamEvent::MessageStop => {
                    log::debug!("[stream] received MessageStop");
                    *stream_done = true;
                }
                StreamEvent::Error { error_type, message } => {
                    log::error!("[stream] SSE error [{}]: {}", error_type, message);
                    *stream_done = true;
                }
                StreamEvent::Ping => {
                    log::trace!("[stream] ping");
                }
                _ => {}
            }
        }

        if *stream_done {
            break;
        }
    }
}

/// The main async entry point — runs inside the cooperative executor.
///
/// Phase 1 goal: boot to a working terminal with keyboard input echoed
/// to serial output, proving the full stack works end-to-end.
///
/// Phase 2 goal: initialize the network stack, obtain an IP via DHCP,
/// resolve DNS, and establish a TCP connection.
async fn main_async() {
    log::info!("[main] async runtime started");
    log::info!("[main] ClaudioOS Phase 1 — Boot to Terminal");
    log::info!("[main] ClaudioOS Phase 2 — Networking");

    // ── Phase 2: Network stack initialization ───────────────────────

    // Step 1: Find the VirtIO-net PCI device (or fall back to e1000).
    let nic_dev = pci::find_device(0x1AF4, 0x1000)
        .or_else(|| {
            log::info!("[main] no VirtIO-net found, trying e1000...");
            pci::find_device(0x8086, 0x100E)
        });

    match nic_dev {
        None => {
            log::warn!("[main] no supported NIC found — skipping networking");
        }
        Some(dev) => {
            log::info!(
                "[main] NIC found: vendor={:#06x} device={:#06x} io_base={:#x} irq={}",
                dev.vendor_id,
                dev.device_id,
                dev.io_base(),
                dev.irq_line,
            );

            let phys_mem_offset = PHYS_MEM_OFFSET.load(Ordering::Relaxed);

            let pci_info = claudio_net::PciDeviceInfo {
                io_base: dev.io_base(),
                irq_line: dev.irq_line,
            };

            // Step 2-4: Init VirtIO driver + smoltcp + DHCP (busy-poll).
            let stack_result = unsafe {
                claudio_net::init(pci_info, phys_mem_offset, now)
            };

            match stack_result {
                Err(e) => {
                    log::error!("[main] network init failed: {:?}", e);
                }
                Ok(mut stack) => {
                    // Step 5: Log the assigned IP, gateway, DNS servers.
                    if let Some(addr) = stack.ipv4_addr() {
                        log::info!("[main] IP address: {}", addr);
                    }
                    if let Some(gw) = stack.gateway {
                        log::info!("[main] gateway: {}", gw);
                    }
                    for dns in &stack.dns_servers {
                        log::info!("[main] DNS server: {}", dns);
                    }

                    // Step 6: Try to resolve "api.anthropic.com".
                    log::info!("[main] resolving api.anthropic.com...");
                    match claudio_net::dns::resolve(&mut stack, "api.anthropic.com", now) {
                        Err(e) => {
                            log::error!("[main] DNS resolution failed: {:?}", e);
                        }
                        Ok(ip) => {
                            log::info!("[main] api.anthropic.com = {}", ip);

                            // Step 7: Native TLS HTTPS request to Anthropic API!
                            // No proxy needed — embedded-tls does the TLS handshake directly.
                            log::info!("[main] ============================================");
                            log::info!("[main] ClaudioOS Phase 3 — NATIVE TLS + API CALL");
                            log::info!("[main] ============================================");

                            // Check baked-in key first, then try auth relay
                            let mut api_key_buf = alloc::string::String::new();
                            if let Some(key) = option_env!("CLAUDIO_API_KEY") {
                                api_key_buf = alloc::string::String::from(key);
                                log::info!("[auth] using compile-time API key ({} chars)", api_key_buf.len());
                            } else {
                            // Fetch API key from auth relay (run `python tools/auth-relay.py`)
                            log::info!("[auth] no compile-time key, trying auth relay...");
                            let relay_ip = claudio_net::Ipv4Address::new(10, 0, 2, 2);
                            for attempt in 0..30u16 {
                                match claudio_net::tls::tcp_connect(&mut stack, relay_ip, 8444, 49300 + attempt, now) {
                                    Err(_) => {
                                        if attempt % 10 == 0 { log::info!("[auth] waiting for relay... ({})", attempt); }
                                        for _ in 0..500000 { core::hint::spin_loop(); }
                                        continue;
                                    }
                                    Ok(h) => {
                                        let req = claudio_net::http::HttpRequest::get("10.0.2.2:8444", "/token")
                                            .header("Connection", "close").to_bytes();
                                        if claudio_net::tls::tcp_send(&mut stack, h, &req, now).is_ok() {
                                            let mut buf = alloc::vec![0u8; 4096];
                                            let mut total = 0;
                                            for _ in 0..30 {
                                                match claudio_net::tls::tcp_recv(&mut stack, h, &mut buf[total..], now) {
                                                    Ok(0) => break,
                                                    Ok(n) => { total += n; if claudio_net::http::HttpResponse::parse(&buf[..total]).is_ok() { break; } }
                                                    Err(_) => break,
                                                }
                                            }
                                            claudio_net::tls::tcp_close(&mut stack, h);
                                            if let Ok(resp) = claudio_net::http::HttpResponse::parse(&buf[..total]) {
                                                if resp.status == 200 {
                                                    if let Ok(body) = core::str::from_utf8(&resp.body) {
                                                        let needle = if body.contains("\"api_key\": \"") { "\"api_key\": \"" } else { "\"api_key\":\"" };
                                                        if let Some(s) = body.find(needle) {
                                                            let rest = &body[s + needle.len()..];
                                                            if let Some(e) = rest.find('"') {
                                                                api_key_buf = alloc::string::String::from(&rest[..e]);
                                                                log::info!("[auth] token: {}...{} ({} chars)", &api_key_buf[..6], &api_key_buf[api_key_buf.len()-4..], api_key_buf.len());
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        } else { claudio_net::tls::tcp_close(&mut stack, h); }
                                        for _ in 0..500000 { core::hint::spin_loop(); }
                                    }
                                }
                            }
                            } // end else (no compile-time key)

                            let api_key: &str = &api_key_buf;
                            if api_key.is_empty() {
                                log::warn!("[auth] no API key — run `python tools/auth-relay.py`");
                            } else {
                                // Streaming API call via TLS proxy.
                                // Run `python tools/tls-proxy.py 8443` on host.
                                log::info!("[main] making STREAMING API request via TLS proxy...");
                                let body = alloc::format!(
                                    r#"{{"model":"claude-haiku-4-5-20251001","max_tokens":256,"stream":true,"messages":[{{"role":"user","content":"Say hi from bare metal in 10 words"}}]}}"#
                                );
                                let req = claudio_net::http::HttpRequest::post(
                                    "api.anthropic.com", "/v1/messages", body.into_bytes(),
                                )
                                .header("Content-Type", "application/json")
                                .header("x-api-key", api_key)
                                .header("anthropic-version", "2023-06-01")
                                .header("Accept", "text/event-stream")
                                .header("Connection", "close");

                                // Connect to TLS proxy on host
                                let proxy = claudio_net::Ipv4Address::new(10, 0, 2, 2);
                                static PORT_CTR: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(50000);
                                let lp = PORT_CTR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                                match claudio_net::tls::tcp_connect(&mut stack, proxy, 8443, lp, now) {
                                    Err(e) => log::error!("[main] proxy connect failed: {:?}. Run: python tools/tls-proxy.py 8443", e),
                                    Ok(h) => {
                                        let bytes = req.to_bytes();
                                        log::info!("[main] sending {} bytes to proxy...", bytes.len());
                                        if let Err(e) = claudio_net::tls::tcp_send(&mut stack, h, &bytes, now) {
                                            log::error!("[main] send failed: {:?}", e);
                                        } else {
                                            log::info!("[main] send complete, reading streaming response...");
                                            log::info!("[main] (proxy is doing TLS upstream to api.anthropic.com — this takes a few seconds)");

                                            // Phase 1: Read until we have the full HTTP headers
                                            let mut header_buf = alloc::vec![0u8; 4096];
                                            let mut header_total = 0usize;
                                            let mut headers_parsed = false;
                                            let mut body_start = 0usize;
                                            let mut http_status = 0u16;

                                            for _ in 0..200 {
                                                match claudio_net::tls::tcp_recv(&mut stack, h, &mut header_buf[header_total..], now) {
                                                    Ok(0) => break,
                                                    Ok(n) => {
                                                        header_total += n;
                                                        // Check if we have the full header block (\r\n\r\n)
                                                        if let Some(pos) = header_buf[..header_total].windows(4).position(|w| w == b"\r\n\r\n") {
                                                            body_start = pos + 4;
                                                            // Parse status from first line
                                                            let hdr_str = core::str::from_utf8(&header_buf[..pos]).unwrap_or("");
                                                            http_status = hdr_str.split(' ').nth(1)
                                                                .and_then(|s| s.parse::<u16>().ok())
                                                                .unwrap_or(0);
                                                            log::info!("[stream] HTTP status: {}", http_status);
                                                            headers_parsed = true;
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        log::error!("[stream] recv error reading headers: {:?}", e);
                                                        break;
                                                    }
                                                }
                                            }

                                            if !headers_parsed {
                                                log::error!("[stream] failed to receive HTTP headers");
                                            } else if http_status != 200 {
                                                // Non-200: read the rest as error body
                                                let error_body = &header_buf[body_start..header_total];
                                                if let Ok(text) = core::str::from_utf8(error_body) {
                                                    log::error!("[stream] API error (HTTP {}): {}", http_status, text);
                                                }
                                            } else {
                                                // Phase 2: Stream SSE events token-by-token
                                                log::info!("[stream] ============================================");
                                                log::info!("[stream]   STREAMING CLAUDE'S RESPONSE (token by token)");
                                                log::info!("[stream] ============================================");

                                                use claudio_api::streaming::{StreamParser, StreamAccumulator, StreamEvent, Delta};
                                                let mut parser = StreamParser::new();
                                                let mut accumulator = StreamAccumulator::new();
                                                let mut stream_done = false;

                                                // Buffer for raw chunked-encoded bytes not yet decoded
                                                let mut chunk_buf = alloc::vec::Vec::<u8>::new();

                                                // Seed with any body bytes already read with headers
                                                if header_total > body_start {
                                                    chunk_buf.extend_from_slice(&header_buf[body_start..header_total]);
                                                }

                                                // Process any complete chunks already buffered
                                                decode_and_feed_chunks(&mut chunk_buf, &mut parser, &mut accumulator, &mut stream_done);

                                                // Phase 3: Read more data incrementally from the TCP stream
                                                let mut recv_buf = [0u8; 4096];
                                                if !stream_done {
                                                    for _ in 0..2000 {
                                                        match claudio_net::tls::tcp_recv(&mut stack, h, &mut recv_buf, now) {
                                                            Ok(0) => {
                                                                log::debug!("[stream] connection closed (EOF)");
                                                                break;
                                                            }
                                                            Ok(n) => {
                                                                chunk_buf.extend_from_slice(&recv_buf[..n]);
                                                                decode_and_feed_chunks(&mut chunk_buf, &mut parser, &mut accumulator, &mut stream_done);
                                                                if stream_done { break; }
                                                            }
                                                            Err(e) => {
                                                                log::warn!("[stream] recv error: {:?}", e);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }

                                                // Drain any remaining data in the parser buffer
                                                let final_events = parser.finish();
                                                for event in &final_events {
                                                    accumulator.process(event);
                                                    if let StreamEvent::ContentBlockDelta { delta: Delta::TextDelta { text }, .. } = event {
                                                        crate::serial_print!("{}", text);
                                                    }
                                                }

                                                crate::serial_print!("\r\n");
                                                log::info!("[stream] ============================================");
                                                log::info!("[stream] stream complete — stop_reason: {:?}", accumulator.stop_reason);
                                                log::info!("[stream] output tokens: {}", accumulator.output_tokens);
                                                log::info!("[stream] full text: {}", accumulator.text);
                                                log::info!("[stream] ============================================");
                                            }
                                        }
                                        claudio_net::tls::tcp_close(&mut stack, h);
                                    }
                                }
                                // ── Phase 4: Multi-agent dashboard ───────────────
                                log::info!("[main] ============================================");
                                log::info!("[main] ClaudioOS Phase 4 — MULTI-AGENT DASHBOARD");
                                log::info!("[main] ============================================");
                                log::info!("[main] Ctrl+B prefix for pane commands:");
                                log::info!("[main]   \" = split horizontal");
                                log::info!("[main]   %% = split vertical");
                                log::info!("[main]   n = focus next, p = focus prev");
                                log::info!("[main]   c = new agent, x = close pane");

                                let fb_w = framebuffer::width();
                                let fb_h = framebuffer::height();
                                dashboard::run_dashboard(
                                    &mut stack,
                                    api_key,
                                    fb_w,
                                    fb_h,
                                    now,
                                ).await;

                                // run_dashboard never returns in normal operation,
                                // but if it does, fall through to the simple loop.
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Fallback: simple keyboard echo loop (no networking) ──────────
    log::info!("[main] falling back to simple keyboard echo loop");

    let fb_w = framebuffer::width();
    let fb_h = framebuffer::height();
    log::info!("[main] setting up terminal layout ({}x{} pixels)", fb_w, fb_h);

    let mut draw_target = terminal::FramebufferDrawTarget;
    let mut layout = claudio_terminal::Layout::new(fb_w, fb_h);

    {
        let pane = layout.focused_pane_mut();
        pane.write_str("\x1b[96mClaudioOS v0.1.0\x1b[0m — \x1b[93mBare Metal AI Agent Terminal\x1b[0m\r\n");
        pane.write_str("\x1b[90m────────────────────────────────────────────────────\x1b[0m\r\n");
        pane.write_str("\r\n");
        pane.write_str("  \x1b[32mPhase 1\x1b[0m: Boot to terminal ............. \x1b[92mOK\x1b[0m\r\n");
        pane.write_str("  \x1b[33mPhase 2\x1b[0m: Networking ................... \x1b[91mN/A\x1b[0m\r\n");
        pane.write_str("\r\n");
        pane.write_str("\x1b[90mNo network/API key — keyboard echo mode.\x1b[0m\r\n");
        pane.write_str("\x1b[97m$ \x1b[0m");
    }

    layout.render_all(&mut draw_target);

    let stream = keyboard::ScancodeStream::new();
    loop {
        let key = stream.next_key().await;
        match key {
            pc_keyboard::DecodedKey::Unicode(c) => {
                crate::serial_print!("{}", c);
                let pane = layout.focused_pane_mut();
                if c == '\n' {
                    pane.write_str("\r\n\x1b[97m$ \x1b[0m");
                } else if c == '\u{8}' {
                    pane.write_str("\x08 \x08");
                } else {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    pane.write_str(s);
                }
                layout.render_all(&mut draw_target);
            }
            pc_keyboard::DecodedKey::RawKey(k) => {
                log::trace!("[kbd] raw key: {:?}", k);
            }
        }
    }
}

/// Halt loop — used after panic or when nothing else to do.
pub fn halt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

/// Panic handler — renders red error to framebuffer + serial.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Print to serial unconditionally, bypassing locks
    serial::force_println!("\n!!! KERNEL PANIC !!!");
    serial::force_println!("{}", info);

    halt_loop()
}
