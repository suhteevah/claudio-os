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

mod executor;
mod framebuffer;
mod gdt;
mod interrupts;
mod keyboard;
mod logger;
mod memory;
mod pci;
mod serial;

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
                                // API call via TLS proxy (native TLS handshake has a bug — WIP).
                                // Run `python tools/tls-proxy.py 8443` on host.
                                log::info!("[main] making API request via TLS proxy...");
                                let body = alloc::format!(
                                    r#"{{"model":"claude-haiku-4-5-20251001","max_tokens":20,"messages":[{{"role":"user","content":"Say hi from bare metal in 10 words"}}]}}"#
                                );
                                let req = claudio_net::http::HttpRequest::post(
                                    "api.anthropic.com", "/v1/messages", body.into_bytes(),
                                )
                                .header("Content-Type", "application/json")
                                .header("x-api-key", api_key)
                                .header("anthropic-version", "2023-06-01")
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
                                            // Aggressively flush — poll many times with HLT between
                                            // to ensure the TCP segments actually reach the proxy
                                            log::info!("[main] flushing send buffer...");
                                            for _ in 0..100 {
                                                stack.iface.poll(now(), &mut stack.device, &mut stack.sockets);
                                                x86_64::instructions::interrupts::enable_and_hlt();
                                                x86_64::instructions::interrupts::disable();
                                            }
                                            x86_64::instructions::interrupts::enable();
                                            log::info!("[main] reading response...");
                                            // Read response until connection closes
                                            let mut buf = alloc::vec![0u8; 32768];
                                            let mut total = 0;
                                            for _ in 0..500 {
                                                match claudio_net::tls::tcp_recv(&mut stack, h, &mut buf[total..], now) {
                                                    Ok(0) => break,
                                                    Ok(n) => { total += n; }
                                                    Err(_) => break,
                                                }
                                            }
                                            if total > 0 {
                                                // Find body after \r\n\r\n
                                                let data = &buf[..total];
                                                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                                                    let body_raw = &data[pos+4..];
                                                    let body_decoded = claudio_net::http::decode_chunked(body_raw)
                                                        .unwrap_or_else(|_| body_raw.to_vec());
                                                    // Get status
                                                    let hdr = core::str::from_utf8(&data[..pos]).unwrap_or("");
                                                    let status = hdr.split(' ').nth(1).unwrap_or("?");
                                                    log::info!("[main] HTTP {}", status);
                                                    log::info!("[main] ============================================");
                                                    log::info!("[main]   CLAUDE'S RESPONSE FROM BARE METAL");
                                                    log::info!("[main] ============================================");
                                                    if let Ok(text) = core::str::from_utf8(&body_decoded) {
                                                        log::info!("[main] {}", text);
                                                    }
                                                    log::info!("[main] ============================================");
                                                }
                                            } else {
                                                log::error!("[main] no response received from proxy");
                                            }
                                        }
                                        claudio_net::tls::tcp_close(&mut stack, h);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Auth is handled inside the networking block above (needs TCP).
    // TODO: Move to separate function once networking refactored.

    // ── Keyboard input loop ──────────────────────────────────────────
    log::info!("[main] keyboard input active, type away!");

    let stream = keyboard::ScancodeStream::new();

    loop {
        let key = stream.next_key().await;
        match key {
            pc_keyboard::DecodedKey::Unicode(c) => {
                crate::serial_print!("{}", c);
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
