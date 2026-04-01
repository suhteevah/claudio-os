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

    // ── Phase 0: Enable SSE/SSE2/AVX — required for crypto + memchr ──
    // memchr uses runtime CPUID to detect AVX2 and will crash if AVX
    // isn't enabled in the OS. We enable the full SSE+AVX stack.
    unsafe {
        // CR0: clear EM (bit 2), set MP (bit 1) — enable FPU/SSE
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // clear EM
        cr0 |= 1 << 1;    // set MP
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        // CR4: set OSFXSR (bit 9) + OSXMMEXCPT (bit 10) — enable SSE
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= (1 << 9) | (1 << 10);

        // Check if XSAVE is supported (CPUID.01H:ECX bit 26)
        // If so, enable OSXSAVE in CR4 and set XCR0 for AVX
        // CPUID leaf 1: check XSAVE (ECX bit 26) and AVX (ECX bit 28)
        let cpuid_result = core::arch::x86_64::__cpuid(1).ecx;
        let xsave_supported = (cpuid_result & (1 << 26)) != 0;
        let avx_supported = (cpuid_result & (1 << 28)) != 0;

        if xsave_supported && avx_supported {
            cr4 |= 1 << 18; // OSXSAVE
            core::arch::asm!("mov cr4, {}", in(reg) cr4);

            // XCR0: enable x87 (bit 0) + SSE (bit 1) + AVX (bit 2)
            let xcr0: u64 = (1 << 0) | (1 << 1) | (1 << 2);
            core::arch::asm!(
                "xsetbv",
                in("ecx") 0u32,
                in("edx") (xcr0 >> 32) as u32,
                in("eax") xcr0 as u32,
            );
        } else {
            core::arch::asm!("mov cr4, {}", in(reg) cr4);
        }
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

    // ── Phase 4b: Early framebuffer test render ──────────────────────
    // Render a simple banner to prove the framebuffer + font pipeline works.
    // This runs BEFORE networking, so it's visible even if DHCP/TLS stalls.
    {
        let fb_w = framebuffer::width();
        let fb_h = framebuffer::height();
        if fb_w > 0 && fb_h > 0 {
            let mut draw_target = terminal::FramebufferDrawTarget;
            let mut layout = claudio_terminal::Layout::new(fb_w, fb_h);
            {
                let pane = layout.focused_pane_mut();
                pane.write_str("\x1b[96mClaudioOS v0.1.0\x1b[0m\r\n");
                pane.write_str("\x1b[93mBare Metal AI Agent Terminal\x1b[0m\r\n");
                pane.write_str("\r\n");
                pane.write_str("\x1b[90mBooting... initialising network stack.\x1b[0m\r\n");
            }
            layout.render_all(&mut draw_target);
            log::info!("[boot] early banner rendered to framebuffer");
        }
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

/// The main async entry point — runs inside the cooperative executor.
///
/// Boot sequence:
///   1. Initialize network stack (VirtIO-net + DHCP + DNS)
///   2. Authenticate (compile-time API key or auth relay on gateway:8444)
///   3. Launch the multi-agent dashboard (native TLS to api.anthropic.com)
async fn main_async() {
    log::info!("[main] async runtime started");
    log::info!("[main] ClaudioOS — Boot to Dashboard");

    // ── Step 1: Network stack initialization ──────────────────────────

    // Find the VirtIO-net PCI device (or fall back to e1000).
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

            // Init VirtIO driver + smoltcp + DHCP (busy-poll).
            let stack_result = unsafe {
                claudio_net::init(pci_info, phys_mem_offset, now)
            };

            match stack_result {
                Err(e) => {
                    log::error!("[main] network init failed: {:?}", e);
                }
                Ok(mut stack) => {
                    // Log the assigned IP, gateway, DNS servers.
                    if let Some(addr) = stack.ipv4_addr() {
                        log::info!("[main] IP address: {}", addr);
                    }
                    if let Some(gw) = stack.gateway {
                        log::info!("[main] gateway: {}", gw);
                    }
                    for dns in &stack.dns_servers {
                        log::info!("[main] DNS server: {}", dns);
                    }

                    // Pre-resolve api.anthropic.com to verify DNS works.
                    log::info!("[main] resolving api.anthropic.com...");
                    match claudio_net::dns::resolve(&mut stack, "api.anthropic.com", now) {
                        Err(e) => {
                            log::error!("[main] DNS resolution failed: {:?}", e);
                        }
                        Ok(ip) => {
                            log::info!("[main] api.anthropic.com = {}", ip);
                        }
                    }

                    // ── Step 2: Authentication ────────────────────────────────

                    // Check baked-in key first, then try OAuth, then relay.
                    let mut api_key_buf = alloc::string::String::new();
                    if let Some(key) = option_env!("CLAUDIO_API_KEY") {
                        api_key_buf = alloc::string::String::from(key);
                        log::info!("[auth] using compile-time API key ({} chars)", api_key_buf.len());
                    } else {
                        // ── Try OAuth: fetch Anthropic console via native HTTPS ──
                        log::info!("[oauth] ============================================");
                        log::info!("[oauth] ATTEMPTING BROWSER-BASED OAUTH");
                        log::info!("[oauth] Following redirect chain from console.anthropic.com...");
                        log::info!("[oauth] ============================================");

                        let mut current_host = alloc::string::String::from("console.anthropic.com");
                        let mut current_path = alloc::string::String::from("/settings/keys");
                        let mut cookies = alloc::string::String::new();

                        for redirect_num in 0..10u8 {
                            log::info!("[oauth] [{}/10] GET https://{}{}",
                                redirect_num + 1, current_host, current_path);

                            let seed = interrupts::tick_count() + redirect_num as u64;
                            let mut req = claudio_net::http::HttpRequest::get(
                                &current_host, &current_path,
                            )
                            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) ClaudioOS/0.1")
                            .header("Accept", "text/html,application/xhtml+xml,*/*")
                            .header("Connection", "close");

                            if !cookies.is_empty() {
                                req = req.header("Cookie", &cookies);
                            }

                            match claudio_net::https_request(
                                &mut stack, &current_host, 443,
                                &req.to_bytes(), now, seed,
                            ) {
                            Ok(resp_bytes) => {
                                let resp_str = core::str::from_utf8(&resp_bytes).unwrap_or("<binary>");
                                log::info!("[oauth] got {} bytes", resp_bytes.len());

                                // Collect Set-Cookie headers
                                for line in resp_str.split("\r\n") {
                                    if let Some(rest) = line.strip_prefix("Set-Cookie:").or_else(|| line.strip_prefix("set-cookie:")) {
                                        if let Some(nv) = rest.trim().split(';').next() {
                                            if !cookies.is_empty() { cookies.push_str("; "); }
                                            cookies.push_str(nv);
                                            log::info!("[oauth] cookie: {}", nv);
                                        }
                                    }
                                }

                                // Parse status code
                                let status = resp_str.split(' ').nth(1)
                                    .and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);

                                // Check for redirect
                                if matches!(status, 301 | 302 | 303 | 307 | 308) {
                                    if let Some(loc_line) = resp_str.split("\r\n").find(|l| l.starts_with("Location:") || l.starts_with("location:")) {
                                        let location = loc_line.splitn(2, ':').nth(1).unwrap_or("").trim();
                                        log::info!("[oauth] {} redirect -> {}", status, location);
                                        if let Some(rest) = location.strip_prefix("https://") {
                                            let (host, path) = if let Some(slash) = rest.find('/') {
                                                (&rest[..slash], &rest[slash..])
                                            } else {
                                                (rest, "/")
                                            };
                                            current_host = alloc::string::String::from(host);
                                            current_path = alloc::string::String::from(path);
                                            continue;
                                        }
                                    }
                                }

                                // Final page — show it
                                log::info!("[oauth] ============================================");
                                log::info!("[oauth] FINAL PAGE: HTTP {} on {}{}", status, current_host, current_path);
                                log::info!("[oauth] ============================================");
                                if let Some(pos) = resp_str.find("\r\n\r\n") {
                                    let body = &resp_str[pos + 4..];
                                    let preview = if body.len() > 1500 { &body[..1500] } else { body };
                                    log::info!("[oauth] {}", preview);
                                }
                                log::info!("[oauth] cookies: {}", cookies);
                                break;
                            }
                            Err(e) => {
                                log::error!("[oauth] request failed: {:?}", e);
                                break;
                            }
                        }
                        } // end redirect loop

                        // Fallback: auth relay
                        // Fetch API key from auth relay (plain HTTP to gateway:8444).
                        // Run `python tools/auth-relay.py` on host.
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
                    } // end auth

                    let api_key: &str = &api_key_buf;
                    if api_key.is_empty() {
                        log::warn!("[auth] no API key — run `python tools/auth-relay.py`");
                    } else {
                        // ── Step 2.5: Native TLS end-to-end test ─────────────
                        // Before launching the dashboard, prove that native TLS
                        // works by making ONE minimal API call directly to
                        // api.anthropic.com:443. This is the critical test —
                        // if TlsStream::connect() completes the TLS 1.3
                        // handshake and we get an HTTP response back, native
                        // TLS is confirmed working.
                        log::info!("[tls-test] ============================================");
                        log::info!("[tls-test] NATIVE TLS END-TO-END TEST");
                        log::info!("[tls-test] ============================================");
                        log::info!("[tls-test] Target: api.anthropic.com:443");
                        log::info!("[tls-test] Model: claude-3-haiku-20240307 (max_tokens: 10)");
                        log::info!("[tls-test] This will: DNS resolve -> TCP connect -> TLS 1.3 handshake -> HTTP POST -> recv response");

                        // Test native TLS — the AVX fix should resolve the memchr null deref
                        log::info!("[tls-test] testing native TLS to api.anthropic.com...");
                        let tls_body = br#"{"model":"claude-haiku-4-5-20251001","max_tokens":10,"messages":[{"role":"user","content":"Say hi"}]}"#;
                        let tls_req = claudio_net::http::HttpRequest::post(
                            "api.anthropic.com", "/v1/messages", tls_body.to_vec(),
                        )
                        .header("Content-Type", "application/json")
                        .header("x-api-key", api_key)
                        .header("anthropic-version", "2023-06-01")
                        .header("Connection", "close");
                        let seed = interrupts::tick_count();
                        let native_tls_ok = match claudio_net::https_request(
                            &mut stack, "api.anthropic.com", 443, &tls_req.to_bytes(), now, seed,
                        ) {
                            Ok(resp) => {
                                log::info!("[tls-test] !! NATIVE TLS SUCCESS !! {} bytes", resp.len());
                                if let Ok(s) = core::str::from_utf8(&resp[..resp.len().min(300)]) {
                                    log::info!("[tls-test] {}", s);
                                }
                                true
                            }
                            Err(e) => {
                                log::error!("[tls-test] native TLS failed: {:?}", e);
                                false
                            }
                        };

                        if native_tls_ok {
                            log::info!("[tls-test] ============================================");
                            log::info!("[tls-test] NATIVE TLS: CONFIRMED WORKING");
                            log::info!("[tls-test] ============================================");
                        } else {
                            log::warn!("[tls-test] ============================================");
                            log::warn!("[tls-test] NATIVE TLS: FAILED — check logs above");
                            log::warn!("[tls-test] ============================================");
                        }

                        // ── Step 2.7: Cranelift JIT proof-of-concept ─────────
                        log::info!("[jit-test] testing Cranelift JIT on bare metal...");
                        let jit_ok = claudio_rustc::test_jit();
                        if jit_ok {
                            log::info!("[jit-test] !! CRANELIFT JIT: CONFIRMED WORKING !!");
                        } else {
                            log::warn!("[jit-test] CRANELIFT JIT: FAILED — check logs above");
                        }

                        // ── Step 3: Launch multi-agent dashboard ──────────────
                        log::info!("[main] ============================================");
                        log::info!("[main] ClaudioOS — MULTI-AGENT DASHBOARD");
                        log::info!("[main] ============================================");
                        if native_tls_ok {
                            log::info!("[main] Mode: NATIVE TLS (verified) to api.anthropic.com:443");
                        } else {
                            log::warn!("[main] Mode: Native TLS UNVERIFIED — agents may fail");
                        }
                        log::info!("[main] Ctrl+B prefix for pane commands:");
                        log::info!("[main]   \" = split horizontal");
                        log::info!("[main]   %% = split vertical");
                        log::info!("[main]   n = focus next, p = focus prev");
                        log::info!("[main]   c = new agent, x = close pane");

                        // Register the compile_rust tool handler so agents
                        // can compile Rust code via the host build server.
                        unsafe {
                            agent_loop::init_compile_handler(
                                &mut stack as *mut _,
                                now,
                            );
                        }

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
