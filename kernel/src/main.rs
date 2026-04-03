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

mod acpi_init;
mod agent_loop;
mod conversations;
mod dashboard;
mod executor;
mod filemanager;
mod framebuffer;
mod gdt;
mod init;
mod interrupts;
mod intel_nic;
mod ipc;
mod keyboard;
mod logger;
mod memory;
mod pci;
mod rtc;
mod serial;
mod smp_init;
mod ssh_server;
mod sysmon;
mod terminal;
mod boot_sound;
mod browser;
mod mouse;
mod screensaver;
mod splash;
mod themes;
mod session_manager;
mod nettools;
mod power;
mod touchpad;
mod usb;
mod users;
mod vconsole;
mod clipboard;
mod csprng;
mod firewall;
mod manpages;
mod cron;
mod email;
mod encryption;
mod image_viewer;
mod model_select;
mod notifications;
mod ntp;
mod search;
mod streaming;
mod swap;
mod git;
mod agent_memory;
mod vectordb;
mod linux_compat;

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
    // Request 1920×1080 framebuffer resolution (or the highest available).
    // The bootloader will pick the closest GOP mode that meets these minimums,
    // falling back to a smaller mode if the display doesn't support 1080p.
    #[allow(deprecated)]
    {
        config.frame_buffer.minimum_framebuffer_width = Some(1920);
        config.frame_buffer.minimum_framebuffer_height = Some(1080);
    }
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

    // ── Phase 3c: Real-Time Clock ────────────────────────────────────
    rtc::init();

    // ── Phase 3c2: CSPRNG ────────────────────────────────────────────
    // Initialize the cryptographically secure RNG (needs PIT + RTC for entropy).
    csprng::init();

    // ── Phase 3d: ACPI table discovery ───────────────────────────────
    // Parse ACPI tables for hardware discovery: CPU cores (MADT), power
    // management (FADT), precision timer (HPET), PCIe ECAM (MCFG).
    // Must run after heap init (allocates) but before networking.
    {
        let rsdp_addr = boot_info.rsdp_addr.into_option();
        acpi_init::init(rsdp_addr);
    }

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

    // ── Phase 4b: Boot splash screen + chime ────────────────────────
    // Show the ClaudioOS splash with progress bar and play the boot chime.
    // This runs BEFORE networking, so it's visible even if DHCP/TLS stalls.
    splash::show_splash(splash::BootStage::Hardware);
    boot_sound::boot_chime();

    // ── Phase 4c: Virtual consoles ──────────────────────────────────
    vconsole::init();

    // ── Phase 5: PCI enumeration + device discovery ──────────────────
    log::info!("[boot] starting PCI enumeration...");
    pci::enumerate();
    log::info!("[boot] PCI enumeration complete");

    // ── Phase 5b: USB (xHCI) host controller + keyboard + mouse ───────
    usb::init();
    mouse::init();

    // ── Phase 5c: SMP — boot application processors ─────────────────
    // Uses MADT data from acpi_init to discover CPU cores, configure the
    // Local APIC on the BSP, install AP trampoline at 0x8000, and boot
    // all APs via INIT-SIPI-SIPI. After this, all cores are running.
    smp_init::init();

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

    splash::show_splash(splash::BootStage::Network);

    // ── Step 1: Network stack initialization ──────────────────────────

    // Detect NIC: try VirtIO-net first (QEMU), then Intel NIC (real hardware).
    let virtio_dev = pci::find_device(0x1AF4, 0x1000);

    // Try Intel NIC if no VirtIO-net found.
    let intel_stack = if virtio_dev.is_none() {
        log::info!("[main] no VirtIO-net found, probing for Intel NIC...");
        match intel_nic::init_intel_network(now) {
            Some(Ok(istack)) => {
                log::info!("[main] Intel NIC active — DHCP complete");
                if let Some(addr) = istack.ipv4_addr() {
                    log::info!("[main] Intel NIC IP: {}", addr);
                }
                if let Some(gw) = istack.gateway {
                    log::info!("[main] Intel NIC gateway: {}", gw);
                }
                for dns in &istack.dns_servers {
                    log::info!("[main] Intel NIC DNS: {}", dns);
                }
                Some(istack)
            }
            Some(Err(e)) => {
                log::error!("[main] Intel NIC init failed: {}", e);
                None
            }
            None => {
                log::info!("[main] no Intel NIC found either");
                None
            }
        }
    } else {
        None
    };

    let nic_dev = virtio_dev;

    match nic_dev {
        None if intel_stack.is_none() => {
            log::warn!("[main] no supported NIC found — skipping networking");
        }
        None => {
            // Intel NIC path — network is up via IntelNetworkStack.
            // Full API client integration (DNS, TLS, HTTPS) requires making
            // claudio_net::NetworkStack generic over the smoltcp Device type.
            // For now, the Intel NIC has DHCP and IP connectivity.
            log::info!("[main] Intel NIC networking active");
            log::info!("[main] NOTE: Full API client requires generic NetworkStack");
        }
        Some(dev) => {
            log::info!(
                "[main] VirtIO-net found: vendor={:#06x} device={:#06x} io_base={:#x} irq={}",
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

                    splash::show_splash(splash::BootStage::Authenticating);

                    // ── Step 2: Authentication ────────────────────────────────

                    // Check for saved session via QEMU fw_cfg, then compile-time, then OAuth
                    let mut api_key_buf = alloc::string::String::new();
                    let mut session_cookie_buf = alloc::string::String::new();
                    let mut saved_conv_id = alloc::string::String::new();

                    // Try reading session from QEMU fw_cfg (opt/claudio/session)
                    {
                        // fw_cfg: enumerate entries to find our key
                        // Selector port: 0x510, Data port: 0x511
                        // Entry 0x0000 = signature, 0x0001 = count
                        // File directory at selector 0x0019
                        log::info!("[auth] checking fw_cfg for saved session...");
                        unsafe {
                            let mut sel = x86_64::instructions::port::Port::<u16>::new(0x510);
                            let mut data = x86_64::instructions::port::Port::<u8>::new(0x511);

                            // Read file directory (selector 0x0019)
                            sel.write(0x0019);
                            // First 4 bytes = number of files (big-endian)
                            let count = ((data.read() as u32) << 24)
                                | ((data.read() as u32) << 16)
                                | ((data.read() as u32) << 8)
                                | (data.read() as u32);
                            log::debug!("[auth] fw_cfg: {} files", count);

                            let mut found_selector: Option<u16> = None;
                            let mut found_size: u32 = 0;

                            for _ in 0..count.min(64) {
                                // Each entry: 4 bytes size, 2 bytes select, 2 bytes reserved, 56 bytes name
                                let size = ((data.read() as u32) << 24)
                                    | ((data.read() as u32) << 16)
                                    | ((data.read() as u32) << 8)
                                    | (data.read() as u32);
                                let select = ((data.read() as u16) << 8) | (data.read() as u16);
                                let _reserved = ((data.read() as u16) << 8) | (data.read() as u16);
                                let mut name_buf = [0u8; 56];
                                for b in name_buf.iter_mut() { *b = data.read(); }
                                let name_len = name_buf.iter().position(|&b| b == 0).unwrap_or(56);
                                let name = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("");

                                if name == "opt/claudio/session" {
                                    log::info!("[auth] fw_cfg: found session file (selector=0x{:04x}, {} bytes)", select, size);
                                    found_selector = Some(select);
                                    found_size = size;
                                    // Skip remaining entries
                                    break;
                                }
                            }

                            if let Some(sel_val) = found_selector {
                                // Read the session data
                                sel.write(sel_val);
                                let mut session_data = alloc::vec::Vec::with_capacity(found_size as usize);
                                for _ in 0..found_size {
                                    session_data.push(data.read());
                                }
                                if let Ok(s) = core::str::from_utf8(&session_data) {
                                    let trimmed = s.trim();
                                    if !trimmed.is_empty() {
                                        // Format: "cookie\nconv_id" or just "cookie"
                                        let mut lines = trimmed.splitn(2, '\n');
                                        if let Some(cookie) = lines.next() {
                                            session_cookie_buf = alloc::string::String::from(cookie.trim());
                                        }
                                        if let Some(conv) = lines.next() {
                                            saved_conv_id = alloc::string::String::from(conv.trim());
                                            log::info!("[auth] loaded saved conversation: {}", saved_conv_id);
                                        }
                                        log::info!("[auth] loaded saved session from fw_cfg ({} bytes)", trimmed.len());
                                    }
                                }
                            }
                        }
                    }

                    if !session_cookie_buf.is_empty() {
                        log::info!("[auth] using saved session cookie");
                    } else if let Some(ssid) = option_env!("CLAUDIO_SESSION") {
                        session_cookie_buf = alloc::format!("sessionKey={}", ssid);
                        log::info!("[auth] using compile-time claude.ai session cookie");
                    } else if let Some(key) = option_env!("CLAUDIO_API_KEY") {
                        api_key_buf = alloc::string::String::from(key);
                        log::info!("[auth] using compile-time API key ({} chars)", api_key_buf.len());
                    } else {
                        // ── SSO OAuth: load /login for cookies → send email → verify code ──
                        log::info!("[oauth] ============================================");
                        log::info!("[oauth]   ClaudioOS SSO Authentication");
                        log::info!("[oauth] ============================================");

                        // Generate a device ID (UUID v4 format) and set initial cookies
                        let tick = interrupts::tick_count();
                        let device_id = alloc::format!(
                            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
                            tick as u32, (tick >> 32) as u16, (tick >> 48) as u16 & 0xFFF,
                            0x8000 | ((tick >> 60) as u16 & 0x3FFF),
                            tick.wrapping_mul(0x5DEECE66D)
                        );
                        let mut session_cookies = alloc::format!("anthropic-device-id={}", device_id);
                        log::info!("[oauth] device-id: {}", device_id);

                        log::info!("[oauth] Enter your email to sign in:");
                        log::info!("[oauth] Type your email and press Enter:");

                        // Read email from SERIAL PORT (not PS/2 keyboard)
                        // QEMU -nographic sends typed chars to serial (0x3F8)
                        let mut email = alloc::string::String::new();
                        loop {
                            // Poll serial port for incoming character
                            let c: u8 = unsafe {
                                let mut lsr = x86_64::instructions::port::Port::<u8>::new(0x3F8 + 5);
                                // Wait for data ready (bit 0 of LSR)
                                loop {
                                    if lsr.read() & 1 != 0 { break; }
                                    core::hint::spin_loop(); // spin_loop, not hlt (serial IRQ4 not unmasked)
                                }
                                x86_64::instructions::port::Port::<u8>::new(0x3F8).read()
                            };
                            match c {
                                b'\r' | b'\n' => {
                                    if !email.is_empty() {
                                        // Echo newline
                                        unsafe {
                                            let mut p = x86_64::instructions::port::Port::<u8>::new(0x3F8);
                                            p.write(b'\r'); p.write(b'\n');
                                        }
                                        break;
                                    }
                                }
                                0x7F | 8 => { // DEL or backspace
                                    if email.pop().is_some() {
                                        unsafe {
                                            let mut p = x86_64::instructions::port::Port::<u8>::new(0x3F8);
                                            p.write(8); p.write(b' '); p.write(8);
                                        }
                                    }
                                }
                                c if c >= 0x20 && c < 0x7F => {
                                    email.push(c as char);
                                    // Echo character
                                    unsafe { x86_64::instructions::port::Port::<u8>::new(0x3F8).write(c); }
                                }
                                _ => {}
                            }
                        }
                        log::info!("[oauth] email: {}", email);

                        // Helper: parse HTTP status code from response
                        fn parse_status(resp: &str) -> u16 {
                            // "HTTP/1.1 302 Found\r\n..."
                            if let Some(line) = resp.lines().next() {
                                let parts: alloc::vec::Vec<&str> = line.split_whitespace().collect();
                                if parts.len() >= 2 { return parts[1].parse().unwrap_or(0); }
                            }
                            0
                        }
                        // Helper: extract Location header
                        fn parse_location(resp: &str) -> Option<alloc::string::String> {
                            for line in resp.split("\r\n") {
                                if let Some(rest) = line.strip_prefix("Location:").or_else(|| line.strip_prefix("location:")) {
                                    return Some(alloc::string::String::from(rest.trim()));
                                }
                            }
                            None
                        }
                        // Helper: collect cookies from response into cookie jar
                        fn collect_cookies(resp: &str, jar: &mut alloc::string::String) {
                            for line in resp.split("\r\n") {
                                if let Some(rest) = line.strip_prefix("Set-Cookie:").or_else(|| line.strip_prefix("set-cookie:")) {
                                    if let Some(nv) = rest.trim().split(';').next() {
                                        if !jar.is_empty() { jar.push_str("; "); }
                                        jar.push_str(nv);
                                    }
                                }
                            }
                        }
                        // Helper: extract body from HTTP response
                        fn extract_body(resp_bytes: &[u8]) -> alloc::vec::Vec<u8> {
                            let resp = core::str::from_utf8(resp_bytes).unwrap_or("");
                            if let Some(pos) = resp.find("\r\n\r\n") {
                                let raw = &resp_bytes[pos + 4..];
                                claudio_net::http::decode_chunked(raw).unwrap_or_else(|_| raw.to_vec())
                            } else {
                                alloc::vec::Vec::new()
                            }
                        }

                        // Helper: make HTTPS request with Cloudflare challenge retry
                        // Returns (status, body_bytes, updated_cookies)
                        fn https_with_cf(
                            stack: &mut claudio_net::NetworkStack,
                            host: &str,
                            req_bytes: &[u8],
                            now: fn() -> claudio_net::Instant,
                            seed: u64,
                            cookies: &mut alloc::string::String,
                        ) -> Result<(u16, alloc::vec::Vec<u8>), &'static str> {
                            let resp_bytes = claudio_net::https_request(stack, host, 443, req_bytes, now, seed)
                                .map_err(|_| "https request failed")?;
                            let resp_str = core::str::from_utf8(&resp_bytes).unwrap_or("");
                            let status = parse_status(resp_str);
                            collect_cookies(resp_str, cookies);
                            let body = extract_body(&resp_bytes);

                            log::info!("[oauth] HTTP {} — {} bytes body", status, body.len());

                            // Check for Cloudflare challenge (403/503)
                            if status == 403 || status == 503 {
                                let body_str = core::str::from_utf8(&body).unwrap_or("");
                                if wraith_dom::cloudflare::is_cloudflare_challenge(body_str) {
                                    log::info!("[oauth] Cloudflare challenge detected! Solving with js-lite...");
                                    if let Some(cf_cookie) = wraith_dom::cloudflare::handle_cloudflare_response(
                                        status, &body, host, "/", cookies,
                                    ) {
                                        log::info!("[oauth] Cloudflare solved! Cookie: {}...", &cf_cookie.cookie[..cf_cookie.cookie.len().min(40)]);
                                        if !cookies.is_empty() { cookies.push_str("; "); }
                                        cookies.push_str(&cf_cookie.cookie);
                                        // Retry with the clearance cookie
                                        // We'd need to rebuild the request with new cookies — caller handles
                                        return Ok((status, body));
                                    } else {
                                        log::warn!("[oauth] could not solve Cloudflare challenge");
                                    }
                                }
                            }

                            // Check for redirect
                            if status == 301 || status == 302 || status == 303 || status == 307 {
                                if let Some(location) = parse_location(resp_str) {
                                    log::info!("[oauth] redirect -> {}", location);
                                    // Extract host and path from Location URL
                                    let (redir_host, redir_path) = if let Some(rest) = location.strip_prefix("https://") {
                                        if let Some(slash) = rest.find('/') {
                                            (&rest[..slash], &rest[slash..])
                                        } else {
                                            (rest, "/")
                                        }
                                    } else {
                                        (host, location.as_str())
                                    };
                                    // Follow redirect with GET
                                    let redir_req = claudio_net::http::HttpRequest::get(redir_host, redir_path)
                                        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
                                        .header("Accept", "application/json, text/html")
                                        .header("Accept-Language", "en-US,en;q=0.5")
                                        .header("Cookie", cookies.as_str())
                                        .header("Connection", "close");
                                    let seed2 = seed.wrapping_add(1);
                                    let resp2 = claudio_net::https_request(stack, redir_host, 443, &redir_req.to_bytes(), now, seed2)
                                        .map_err(|_| "redirect request failed")?;
                                    let resp2_str = core::str::from_utf8(&resp2).unwrap_or("");
                                    let status2 = parse_status(resp2_str);
                                    collect_cookies(resp2_str, cookies);
                                    let body2 = extract_body(&resp2);
                                    log::info!("[oauth] redirect result: HTTP {} — {} bytes", status2, body2.len());
                                    return Ok((status2, body2));
                                }
                            }

                            Ok((status, body))
                        }

                        // Step 1: POST /api/auth/send_magic_link
                        log::info!("[oauth] sending magic link request...");
                        let tz_offset = 300i32; // EST = UTC-5 = 300 minutes
                        let body = alloc::format!(
                            r#"{{"email_address":"{}","login_intent":"magic_link","utc_offset":{},"source":"claude"}}"#, email, tz_offset
                        );
                        let seed = interrupts::tick_count();
                        let mut req = claudio_net::http::HttpRequest::post(
                            "claude.ai",
                            "/api/auth/send_magic_link",
                            body.into_bytes(),
                        )
                        .header("Content-Type", "application/json")
                        .header("Origin", "https://claude.ai")
                        .header("Referer", "https://claude.ai/login")
                        .header("Accept", "application/json")
                        .header("Accept-Language", "en-US,en;q=0.5")
                        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
                        .header("anthropic-client-sha", "unknown")
                        .header("anthropic-client-version", "unknown")
                        .header("anthropic-anonymous-id", &device_id)
                        .header("anthropic-device-id", &device_id)
                        .header("Sec-Fetch-Dest", "empty")
                        .header("Sec-Fetch-Mode", "cors")
                        .header("Sec-Fetch-Site", "same-origin")
                        .header("Connection", "close");
                        if !session_cookies.is_empty() {
                            req = req.header("Cookie", &session_cookies);
                        }

                        match https_with_cf(&mut stack, "claude.ai", &req.to_bytes(), now, seed, &mut session_cookies) {
                            Ok((status, body_bytes)) => {
                                let body = core::str::from_utf8(&body_bytes).unwrap_or("{}");
                                log::info!("[oauth] send_magic_link: status={} body={}", status, body);

                                if status == 200 {
                                    // Check for sso_url (SSO redirect)
                                    if let Some(sso_start) = body.find("\"sso_url\":\"") {
                                        let rest = &body[sso_start + 11..];
                                        if let Some(end) = rest.find('"') {
                                            let sso_url = &rest[..end];
                                            log::info!("[oauth] ============================================");
                                            log::info!("[oauth]   SSO AUTHENTICATION REQUIRED");
                                            log::info!("[oauth] ============================================");
                                            log::info!("[oauth] Open this URL on your phone or browser:");
                                            log::info!("[oauth]");
                                            log::info!("[oauth]   {}", sso_url);
                                            log::info!("[oauth]");
                                            log::info!("[oauth] After authenticating, press Enter here.");
                                            log::info!("[oauth] ============================================");

                                            // Wait for Enter on serial
                                            loop {
                                                let c: u8 = unsafe {
                                                    let mut lsr = x86_64::instructions::port::Port::<u8>::new(0x3F8 + 5);
                                                    loop { if lsr.read() & 1 != 0 { break; } core::hint::spin_loop(); }
                                                    x86_64::instructions::port::Port::<u8>::new(0x3F8).read()
                                                };
                                                if c == b'\r' || c == b'\n' { break; }
                                            }
                                            log::info!("[oauth] SSO auth flow completed");
                                        }
                                    } else {
                                        // Magic link sent — ask for the code
                                        log::info!("[oauth] ============================================");
                                        log::info!("[oauth]   VERIFICATION CODE SENT!");
                                        log::info!("[oauth]   Check your email: {}", email);
                                        log::info!("[oauth]   Enter the 6-digit code:");
                                        log::info!("[oauth] ============================================");

                                        // Read code from serial
                                        let mut code = alloc::string::String::new();
                                        loop {
                                            let c: u8 = unsafe {
                                                let mut lsr = x86_64::instructions::port::Port::<u8>::new(0x3F8 + 5);
                                                loop { if lsr.read() & 1 != 0 { break; } core::hint::spin_loop(); }
                                                x86_64::instructions::port::Port::<u8>::new(0x3F8).read()
                                            };
                                            match c {
                                                b'\r' | b'\n' => {
                                                    if !code.is_empty() {
                                                        unsafe {
                                                            let mut p = x86_64::instructions::port::Port::<u8>::new(0x3F8);
                                                            p.write(b'\r'); p.write(b'\n');
                                                        }
                                                        break;
                                                    }
                                                }
                                                c if c >= b'0' && c <= b'9' => {
                                                    code.push(c as char);
                                                    unsafe { x86_64::instructions::port::Port::<u8>::new(0x3F8).write(c); }
                                                }
                                                0x7F | 8 => {
                                                    if code.pop().is_some() {
                                                        unsafe {
                                                            let mut p = x86_64::instructions::port::Port::<u8>::new(0x3F8);
                                                            p.write(8); p.write(b' '); p.write(8);
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        log::info!("[oauth] verifying code: {}", code);

                                        // Warm up DNS before verify (socket may have timed out during code entry)
                                        let _ = claudio_net::dns::resolve(&mut stack, "claude.ai", now);

                                        // POST /api/auth/verify_magic_link
                                        let verify_body = alloc::format!(
                                            r#"{{"credentials":{{"method":"code","email_address":"{}","code":"{}"}},"source":"claude","locale":"en-US"}}"#,
                                            email, code
                                        );
                                        log::info!("[oauth] verify body: {}", verify_body);
                                        let seed2 = interrupts::tick_count();
                                        let mut verify_req = claudio_net::http::HttpRequest::post(
                                            "claude.ai",
                                            "/api/auth/verify_magic_link",
                                            verify_body.into_bytes(),
                                        )
                                        .header("Content-Type", "application/json")
                                        .header("Origin", "https://claude.ai")
                                        .header("Referer", "https://claude.ai/login")
                                        .header("Accept", "application/json")
                                        .header("Accept-Language", "en-US,en;q=0.5")
                                        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
                                        .header("anthropic-client-sha", "unknown")
                                        .header("anthropic-client-version", "unknown")
                                        .header("anthropic-anonymous-id", &device_id)
                                        .header("anthropic-device-id", &device_id)
                                        .header("Sec-Fetch-Dest", "empty")
                                        .header("Sec-Fetch-Mode", "cors")
                                        .header("Sec-Fetch-Site", "same-origin")
                                        .header("Connection", "close");
                                        if !session_cookies.is_empty() {
                                            verify_req = verify_req.header("Cookie", &session_cookies);
                                        }

                                        match https_with_cf(&mut stack, "claude.ai", &verify_req.to_bytes(), now, seed2, &mut session_cookies) {
                                            Ok((vstatus, vbody_bytes)) => {
                                                let vbody = core::str::from_utf8(&vbody_bytes).unwrap_or("{}");

                                                if vbody.contains("\"success\":true") || vbody.contains("\"success\": true") {
                                                    log::info!("[oauth] ============================================");
                                                    log::info!("[oauth]   !! AUTHENTICATION SUCCESSFUL !!");
                                                    log::info!("[oauth] ============================================");

                                                    // Extract session cookie (sessionKey or __ssid)
                                                    log::info!("[oauth] cookies: {}", &session_cookies[..session_cookies.len().min(500)]);
                                                    for part in session_cookies.split("; ") {
                                                        if part.starts_with("sessionKey=") || part.starts_with("__ssid=") {
                                                            session_cookie_buf = alloc::string::String::from(part);
                                                            log::info!("[oauth] session cookie acquired: {}...", &part[..part.len().min(50)]);
                                                            break;
                                                        }
                                                    }
                                                    if session_cookie_buf.is_empty() {
                                                        // Use all cookies as fallback
                                                        log::warn!("[oauth] no sessionKey or __ssid found, using all cookies");
                                                        session_cookie_buf = session_cookies.clone();
                                                    }

                                                    // Print save marker for host script to capture
                                                    log::info!("[oauth] SAVE_SESSION:{}", session_cookie_buf);

                                                    // Extract org UUID from response
                                                    if let Some(s) = vbody.find("\"uuid\":\"") {
                                                        let rest = &vbody[s + 8..];
                                                        if let Some(e) = rest.find('"') {
                                                            // Skip account uuid, find org uuid
                                                            if let Some(s2) = rest[e+1..].find("\"uuid\":\"") {
                                                                let rest2 = &rest[e+1+s2+8..];
                                                                if let Some(e2) = rest2.find('"') {
                                                                    let org = &rest2[..e2];
                                                                    if org.contains('-') && org.len() > 30 {
                                                                        log::info!("[oauth] org: {}", org);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    log::error!("[oauth] verify: status={} body={}", vstatus, &vbody[..vbody.len().min(300)]);
                                                }
                                            }
                                            Err(e) => {
                                                log::error!("[oauth] verify failed: {}", e);
                                            }
                                        }
                                    }
                                } else {
                                    log::error!("[oauth] send_magic_link failed: HTTP {} — {}", status, body);
                                }
                            }
                            Err(e) => {
                                log::error!("[oauth] send_magic_link failed: {}", e);
                            }
                        }

                        // If we got a session cookie from OAuth, skip the relay
                        if !session_cookie_buf.is_empty() {
                            log::info!("[oauth] session acquired — skipping auth relay");
                        } else {
                        log::info!("[oauth] falling back to auth relay for API key...");
                        log::info!("[oauth] run: python tools/auth-relay.py");

                        // Old redirect chain removed — SSO flow above handles auth

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
                        } // end relay fallback else
                    } // end auth

                    let api_key: &str = &api_key_buf;
                    let session_cookie: &str = &session_cookie_buf;

                    if !session_cookie.is_empty() {
                        // ── claude.ai Max mode: use session cookie ──────────────
                        log::info!("[claude.ai] ============================================");
                        log::info!("[claude.ai]   CLAUDE.AI MAX MODE");
                        log::info!("[claude.ai] ============================================");
                        log::info!("[claude.ai] Using session cookie for unlimited access");

                        let org_id = "9cb75ae8-c9bb-4ef3-afed-7ff716b22fd3";

                        // Step 1: Create or reuse conversation
                        let conv_uuid = if !saved_conv_id.is_empty() {
                            log::info!("[claude.ai] reusing saved conversation: {}", saved_conv_id);
                            Some(saved_conv_id.clone())
                        } else {
                        log::info!("[claude.ai] creating conversation...");
                        let create_body = br#"{"name":"","project_uuid":null}"#;
                        let create_path = alloc::format!("/api/organizations/{}/chat_conversations", org_id);
                        let create_req = claudio_net::http::HttpRequest::post(
                            "claude.ai", &create_path, create_body.to_vec(),
                        )
                        .header("Content-Type", "application/json")
                        .header("Cookie", session_cookie)
                        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
                        .header("Accept", "application/json")
                        .header("Origin", "https://claude.ai")
                        .header("Referer", "https://claude.ai/new")
                        .header("Connection", "close");
                        let seed = interrupts::tick_count();
                        match claudio_net::https_request(
                            &mut stack, "claude.ai", 443, &create_req.to_bytes(), now, seed,
                        ) {
                            Ok(resp) => {
                                let resp_str = core::str::from_utf8(&resp).unwrap_or("");
                                // Extract body
                                let body = if let Some(pos) = resp_str.find("\r\n\r\n") {
                                    let raw = &resp[pos + 4..];
                                    let decoded = claudio_net::http::decode_chunked(raw).unwrap_or_else(|_| raw.to_vec());
                                    alloc::string::String::from(core::str::from_utf8(&decoded).unwrap_or(""))
                                } else { alloc::string::String::new() };

                                // Extract uuid from response
                                if let Some(s) = body.find("\"uuid\":\"") {
                                    let rest = &body[s + 8..];
                                    if let Some(e) = rest.find('"') {
                                        let uuid = alloc::string::String::from(&rest[..e]);
                                        log::info!("[claude.ai] conversation: {}", uuid);
                                        Some(uuid)
                                    } else { None }
                                } else {
                                    log::error!("[claude.ai] create failed: {}", &body[..body.len().min(200)]);
                                    None
                                }
                            }
                            Err(e) => { log::error!("[claude.ai] create request failed: {:?}", e); None }
                        }
                        }; // end if saved_conv_id

                        if let Some(conv_id) = conv_uuid {
                            // Save conv_id for reuse across reboots
                            if saved_conv_id.is_empty() {
                                log::info!("[oauth] SAVE_CONV:{}", conv_id);
                            }

                            // Step 2: Send a message
                            log::info!("[claude.ai] sending test message...");
                            let test_model = model_select::claude_ai_model_id();
                            let msg_body = alloc::format!(
                                r#"{{"prompt":"Say hello from bare metal! Keep it short.","timezone":"America/New_York","attachments":[],"files":[],"model":"{}","rendering_mode":"messages"}}"#,
                                test_model
                            );
                            let msg_path = alloc::format!("/api/organizations/{}/chat_conversations/{}/completion", org_id, conv_id);
                            let msg_req = claudio_net::http::HttpRequest::post(
                                "claude.ai", &msg_path, msg_body.into_bytes(),
                            )
                            .header("Content-Type", "application/json")
                            .header("Cookie", session_cookie)
                            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
                            .header("Accept", "text/event-stream")
                            .header("Origin", "https://claude.ai")
                            .header("Referer", "https://claude.ai/new")
                            .header("Connection", "close");
                            let seed2 = interrupts::tick_count();
                            match claudio_net::https_request(
                                &mut stack, "claude.ai", 443, &msg_req.to_bytes(), now, seed2,
                            ) {
                                Ok(resp) => {
                                    let resp_str = core::str::from_utf8(&resp).unwrap_or("");
                                    log::info!("[claude.ai] response: {} bytes", resp.len());

                                    // Parse SSE events for text deltas
                                    let mut full_text = alloc::string::String::new();
                                    for line in resp_str.lines() {
                                        if let Some(data) = line.strip_prefix("data: ") {
                                            if data.contains("\"text_delta\"") {
                                                if let Some(s) = data.find("\"text\":\"") {
                                                    let rest = &data[s + 8..];
                                                    if let Some(e) = rest.find('"') {
                                                        full_text.push_str(&rest[..e]);
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if !full_text.is_empty() {
                                        log::info!("[claude.ai] ============================================");
                                        log::info!("[claude.ai]   CLAUDE SAYS: {}", full_text);
                                        log::info!("[claude.ai] ============================================");
                                        log::info!("[claude.ai]   !! CLAUDE.AI MAX MODE WORKING !!");
                                    } else {
                                        // Show raw response for debugging
                                        if let Some(pos) = resp_str.find("\r\n\r\n") {
                                            let body = &resp_str[pos + 4..resp_str.len().min(pos + 500)];
                                            log::info!("[claude.ai] raw: {}", body);
                                        }
                                    }
                                }
                                Err(e) => { log::error!("[claude.ai] completion failed: {:?}", e); }
                            }

                            // Set auth mode for the agent loop / dashboard
                            unsafe {
                                agent_loop::set_auth_mode(agent_loop::AuthMode::ClaudeAi {
                                    session_cookie: alloc::string::String::from(session_cookie),
                                    org_id: alloc::string::String::from(org_id),
                                    conv_id: conv_id.clone(),
                                });
                            }
                            log::info!("[claude.ai] auth mode set — launching dashboard");

                            // Initialize session manager for automatic token refresh.
                            session_manager::init(
                                alloc::string::String::from(session_cookie),
                                alloc::string::String::from(org_id),
                                conv_id.clone(),
                            );

                            // Register compile handler + VFS/command tool handlers
                            unsafe {
                                agent_loop::init_compile_handler(
                                    &mut stack as *mut _,
                                    now,
                                );
                                agent_loop::init_tool_handlers();
                            }

                            splash::show_splash(splash::BootStage::Ready);
                            splash::hide_splash();

                            // Launch multi-agent dashboard on claude.ai Max
                            let fb_w = framebuffer::width();
                            let fb_h = framebuffer::height();
                            dashboard::run_dashboard(
                                &mut stack,
                                "", // no API key needed — using claude.ai session
                                fb_w,
                                fb_h,
                                now,
                            ).await;
                        }
                    } else if api_key.is_empty() {
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
                        // can compile Rust code via the host build server,
                        // plus VFS + command tool handlers.
                        unsafe {
                            agent_loop::init_compile_handler(
                                &mut stack as *mut _,
                                now,
                            );
                            agent_loop::init_tool_handlers();
                        }

                        // Start the SSH server on port 22 (polled from dashboard loop).
                        ssh_server::start_ssh_server(&mut stack, now);

                        splash::show_splash(splash::BootStage::Ready);
                        splash::hide_splash();

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
