//! ClaudioOS — Bare-metal Rust OS for AI coding agents.
//!
//! This is the kernel binary crate: the very first Rust code that runs after
//! the UEFI bootloader hands off control.  It is `#![no_std]` + `#![no_main]`
//! -- there is no libc, no POSIX, no runtime.  Everything is built from scratch.
//!
//! # Boot sequence (in order of execution)
//!
//! ```text
//! UEFI firmware
//!   -> bootloader crate (v0.11) -- sets up page tables, identity map, GOP framebuffer
//!     -> kernel_main()          -- Phase -1..6: hardware init, heap, interrupts, PCI, SMP
//!       -> post_stack_switch()  -- switches to heap-allocated 4 MiB stack
//!         -> main_async()       -- async executor starts, networking, auth, dashboard
//! ```
//!
//! # Key design decisions
//!
//! - **Single address space**: no kernel/user boundary, no syscalls, no process
//!   isolation.  Every agent session is an async task in the same address space.
//! - **Cooperative multitasking**: an interrupt-driven async executor runs all
//!   tasks.  `hlt` when idle for power savings.
//! - **Native TLS**: HTTPS to api.anthropic.com is done directly from kernel
//!   code via `embedded-tls` + `smoltcp`.  No userland networking stack.
//! - **No JavaScript**: the Anthropic Messages API is called via raw HTTP/1.1
//!   POST + SSE streaming.  No Node.js, no npm, no V8.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod acpi_init;
mod agent_loop;
mod conversations;
mod dashboard;
mod disks;
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
mod storage;
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
mod usb_storage;
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
mod win32_compat;
mod dotnet_compat;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use limine::request::{
    FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, RequestsEndMarker,
    RequestsStartMarker, RsdpRequest, StackSizeRequest,
};
use limine::BaseRevision;

/// Physical memory offset provided by the bootloader, stored globally so that
/// subsystems initialised after boot (e.g. networking, xHCI, Intel NIC) can
/// translate between virtual and physical addresses.
///
/// The Limine HHDM maps all physical memory at `virtual = physical + offset`.
/// This means any physical address P is accessible at virtual address P + offset.
/// This is essential for MMIO register access (PCI BARs, xHCI, E1000) and DMA
/// buffer address translation.
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Return the bootloader-supplied physical memory offset.
pub fn phys_mem_offset() -> u64 {
    PHYS_MEM_OFFSET.load(Ordering::Relaxed)
}

// ── Limine requests ────────────────────────────────────────────────────
//
// These static requests are scanned for by the Limine bootloader before it
// hands off control to us. The `#[used]` + `#[link_section]` attributes
// place them in the `.requests` section so the linker script can bracket
// them with start/end markers. The `BaseRevision` tag tells the bootloader
// which revision of the protocol we understand (3 = latest as of limine 0.5).
//
// NOTE: these must be `#[used]` or the compiler may strip them in release
// builds where the kernel never references them by name.

/// Base revision tag — asks for the latest Limine protocol revision. Without
/// this tag, Limine assumes revision 0 which is missing HHDM responses etc.
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(2);

/// Stack size request — ask for a 128 KiB initial stack. Interrupt handlers
/// + log formatting are stack-heavy; the 64 KiB default would overflow.
/// (We still switch to a 4 MiB heap-allocated stack later in boot for the
/// async executor + TLS handshake, but that happens after the heap is up.)
#[used]
#[unsafe(link_section = ".requests")]
static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(128 * 1024);

/// Framebuffer request — we need a GOP framebuffer for the dashboard UI.
#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

/// HHDM request — returns the "higher-half direct map" offset.
/// Equivalent to `bootloader 0.11`'s `physical_memory_offset`.
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

/// Memory map request — returns the bootloader's view of physical RAM.
/// Equivalent to `bootloader 0.11`'s `memory_regions`.
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

/// RSDP request — returns the physical address of the ACPI RSDP table.
#[used]
#[unsafe(link_section = ".requests")]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

/// Module request — lets us pick up extra files (e.g. `model.gguf`) that
/// Limine loaded alongside the kernel. Equivalent to bootloader 0.11's
/// `ramdisk_addr`/`ramdisk_len`.
#[used]
#[unsafe(link_section = ".requests")]
static MODULE_REQUEST: ModuleRequest = ModuleRequest::new();

// Start/end markers bracket the .requests section so the bootloader knows
// where to stop scanning. The linker script's KEEP() calls ensure they're
// preserved even in LTO builds.
#[used]
#[unsafe(link_section = ".requests_start_marker")]
static REQUESTS_START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static REQUESTS_END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// Primary kernel entry point — called by Limine after UEFI handoff.
///
/// Limine has already:
/// - Loaded our ELF at the linker-script-specified higher-half address
/// - Set up 4-level paging with the HHDM mapping active
/// - Populated all our request responses
/// - Enabled long mode, SSE, and basic CPU state
///
/// Interrupts are disabled at entry. We pull the boot info we need out of
/// the static request responses at the top of this function and then run
/// the same phase-by-phase init the old bootloader 0.11 path used.
/// Write a zero-terminated byte string to serial port 0x3F8. Used for
/// dead-simple proof-of-life probes before any init.
#[inline(always)]
fn early_serial(msg: &[u8]) {
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in msg {
            port.write(b);
        }
    }
}

/// Cached framebuffer pointer + geometry, populated the first time Phase -2
/// runs. Used by `fb_checkpoint` to draw small status squares below the
/// rainbow bars so we can visually trace how far boot got on real hardware
/// where serial is invisible.
static mut FB_CHECKPOINT_BUF: *mut u8 = core::ptr::null_mut();
static mut FB_CHECKPOINT_PITCH: usize = 0;
static mut FB_CHECKPOINT_BPP: usize = 0;
static mut FB_CHECKPOINT_WIDTH: usize = 0;
static mut FB_CHECKPOINT_HEIGHT: usize = 0;

/// Paint the Nth 20x20 checkpoint square in a horizontal row directly under
/// the rainbow bars. `color_rgb` is (R, G, B). No-op if the framebuffer
/// pointer hasn't been cached yet.
///
/// The row starts at y=260 (just below the 6*40-px bars). Squares are laid
/// out left-to-right with 8 px gaps, so up to ~40 fit on a 1280-wide panel.
pub fn fb_checkpoint(n: usize, color_rgb: (u8, u8, u8)) {
    unsafe {
        if FB_CHECKPOINT_BUF.is_null() {
            return;
        }
        let pitch = FB_CHECKPOINT_PITCH;
        let bpp = FB_CHECKPOINT_BPP;
        let width = FB_CHECKPOINT_WIDTH;
        let height = FB_CHECKPOINT_HEIGHT;
        let buf_len = pitch * height;
        let sq = 20usize;
        let gap = 8usize;
        let y0 = 260usize;
        let x0 = 10 + n * (sq + gap);
        if x0 + sq > width || y0 + sq > height {
            return;
        }
        for yy in 0..sq {
            for xx in 0..sq {
                let off = (y0 + yy) * pitch + (x0 + xx) * bpp;
                if off + 3 <= buf_len {
                    let p = FB_CHECKPOINT_BUF.add(off);
                    core::ptr::write_volatile(p, color_rgb.2); // B
                    core::ptr::write_volatile(p.add(1), color_rgb.1); // G
                    core::ptr::write_volatile(p.add(2), color_rgb.0); // R
                    if bpp > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Proof-of-life immediately at handoff — confirms Limine jumped here
    // before we touch framebuffer or request responses.
    early_serial(b"[claudio] Limine handoff reached\r\n");

    // Enable SSE/SSE2/XSAVE/AVX before anything else. Limine hands off with
    // SSE disabled in CR0/CR4, which causes #UD for any generated SSE insn
    // (including ones emitted by the `limine` crate's Framebuffer accessors
    // and by Rust's `memchr` auto-vectorization). We need this to be the
    // very first thing in Rust code so even the RGB proof-of-life bars
    // below can run on real hardware.
    //
    // SAFETY: ring 0, writing CR0/CR4/XCR0 per Intel SDM Vol. 3A §13.5.
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // clear EM
        cr0 |= 1 << 1;    // set MP
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= (1 << 9) | (1 << 10); // OSFXSR + OSXMMEXCPT

        let cpuid = core::arch::x86_64::__cpuid(1).ecx;
        let xsave = (cpuid & (1 << 26)) != 0;
        let avx = (cpuid & (1 << 28)) != 0;
        if xsave && avx {
            cr4 |= 1 << 18; // OSXSAVE
            core::arch::asm!("mov cr4, {}", in(reg) cr4);
            let xcr0: u64 = 0b111; // x87 + SSE + AVX
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

    early_serial(b"[claudio] SSE enabled\r\n");
    kernel_main()
}

fn kernel_main() -> ! {
    // ── Phase -2: Visual proof-of-life on framebuffer ──────────────────
    // On real hardware (no serial), this is the only way to prove kernel_main
    // was reached. Paint 6 full-width bands so the user can eyeball progress
    // even before any driver init. A completely black screen after this means
    // Limine's FB response was not provided (run with `KERNEL_PATH=` + no
    // framebuffer in limine.cfg?) or the HHDM-mapped FB address faulted.
    if let Some(fb_resp) = FRAMEBUFFER_REQUEST.get_response() {
        if let Some(fb) = fb_resp.framebuffers().next() {
            let width = fb.width() as usize;
            let height = fb.height() as usize;
            let pitch = fb.pitch() as usize;
            let bpp = ((fb.bpp() as usize) + 7) / 8;
            let addr = fb.addr();
            let buf_len = pitch * height;
            let buf: &mut [u8] =
                unsafe { core::slice::from_raw_parts_mut(addr, buf_len) };

            // Cache for fb_checkpoint() — lets later phases draw status
            // squares below the bars without re-walking Limine's response.
            unsafe {
                FB_CHECKPOINT_BUF = addr;
                FB_CHECKPOINT_PITCH = pitch;
                FB_CHECKPOINT_BPP = bpp;
                FB_CHECKPOINT_WIDTH = width;
                FB_CHECKPOINT_HEIGHT = height;
            }

            // 6 bands, each BAND_H pixels tall, full screen width, distinct
            // colors so a missing band tells you how far we got:
            //   red, orange, yellow, green, cyan, blue
            const BANDS: [[u8; 3]; 6] = [
                [0, 0, 255],   // red
                [0, 128, 255], // orange
                [0, 255, 255], // yellow
                [0, 255, 0],   // green
                [255, 255, 0], // cyan
                [255, 0, 0],   // blue
            ];
            let band_h = 40;
            for (i, c) in BANDS.iter().enumerate() {
                let y0 = i * band_h;
                let y1 = ((i + 1) * band_h).min(height);
                for y in y0..y1 {
                    let row = y * pitch;
                    for x in 0..width {
                        let off = row + x * bpp;
                        if off + 3 <= buf.len() {
                            buf[off] = c[0];
                            buf[off + 1] = c[1];
                            buf[off + 2] = c[2];
                            if bpp > 3 {
                                buf[off + 3] = 0;
                            }
                        }
                    }
                }
                // Emit a serial marker per band so even over QEMU we can
                // confirm the loop ran.
                early_serial(b"[fb-band]\r\n");
            }
            early_serial(b"[fb-done]\r\n");
        } else {
            early_serial(b"[fb-no-framebuffers]\r\n");
        }
    } else {
        early_serial(b"[fb-no-response]\r\n");
    }

    // ── Phase -1: Bare minimum proof-of-life (VGA text mode + raw serial) ──
    // Write directly to serial port 0x3F8 WITHOUT full UART init to prove
    // we actually reached kernel_main. VGA text buffer at 0xB8000 too.
    // SAFETY: Writing to serial port 0x3F8 is a standard x86 I/O operation.
    // At this point we are in kernel mode with full I/O permissions.
    // The UART data register at 0x3F8 is safe to write even without full
    // UART initialization — QEMU's emulated 16550 accepts bytes immediately.
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"[claudio] kernel_main entered\r\n" {
            port.write(b);
        }
    }

    // Visual checkpoint #0 (white) — reached Phase 0 (past bars).
    fb_checkpoint(0, (255, 255, 255));

    // ── Phase 0a: Serial debug output (available immediately) ─────────
    // (SSE/XSAVE was already enabled in `_start` before we touched the
    // framebuffer; nothing to do here.)
    serial::init();
    fb_checkpoint(1, (255, 255, 255));

    // ── Phase 0b: Logger (so all subsequent log::* calls produce output) ──
    logger::init();
    log::info!("[boot] ClaudioOS v{} starting", env!("CARGO_PKG_VERSION"));
    log::info!("[boot] SSE/SSE2 enabled");
    log::info!("[boot] bootloader handed off control");
    fb_checkpoint(2, (255, 255, 255));

    // ── Phase 1: CPU structures ──────────────────────────────────────
    gdt::init();
    log::info!("[boot] GDT initialized with TSS");
    fb_checkpoint(3, (255, 255, 255));

    // ── Phase 2: Memory management ───────────────────────────────────
    // Must initialize heap BEFORE enabling interrupts, because the IDT
    // lazy-init and interrupt handlers may allocate.
    let phys_mem_offset = HHDM_REQUEST
        .get_response()
        .expect("limine must provide HHDM response")
        .offset();

    // Store phys_mem_offset globally so subsystems like networking can use it.
    PHYS_MEM_OFFSET.store(phys_mem_offset, Ordering::Relaxed);

    let memory_map = MEMORY_MAP_REQUEST
        .get_response()
        .expect("limine must provide memory map response")
        .entries();
    memory::init(phys_mem_offset, memory_map);
    log::info!("[boot] heap allocator initialized");
    fb_checkpoint(4, (255, 255, 255));

    // ── Phase 2b: Storage / VFS (needs heap; must precede anything that
    // reads credentials or config through claudio-fs) ───────────────────
    storage::init();
    fb_checkpoint(5, (255, 255, 255));

    // Drain any log lines buffered before the VFS was online and arm the
    // logger's file sink so subsequent log::* calls land in
    // /claudio/logs/kernel.log.
    logger::flush_ring_buffer_to_vfs();
    fb_checkpoint(6, (255, 255, 255));

    // ── Phase 3: Interrupts (needs heap for keyboard queue allocs) ────
    interrupts::init();
    log::info!("[boot] IDT loaded, PIC initialized (interrupts still disabled)");
    fb_checkpoint(7, (255, 255, 255));

    // ── Phase 3b: Keyboard decoder ────────────────────────────────────
    keyboard::init();
    fb_checkpoint(8, (255, 255, 255));

    // ── Phase 3c: Real-Time Clock ────────────────────────────────────
    rtc::init();
    fb_checkpoint(9, (255, 255, 255));

    // ── Phase 3c2: CSPRNG ────────────────────────────────────────────
    // Initialize the cryptographically secure RNG (needs PIT + RTC for entropy).
    csprng::init();
    fb_checkpoint(10, (255, 255, 255));

    // ── Phase 3c3: Local LLM model from bootloader module ────────────
    // Limine's ModuleResponse exposes any files we told it to load alongside
    // the kernel (see limine.conf). We treat any module named "model.gguf"
    // (or matching the substring "gguf") as the local LLM weights and hand
    // them to claudio-llm so the local-model tool handler has a real model
    // to run instead of always erroring out.
    // Phase 3c3: LLM module/VFS model load — ONLY try Limine's module list.
    // The VFS fallback (claudio_fs::read_file) deadlocks on real 12th-gen
    // hardware in a way it doesn't in QEMU; the model isn't required to
    // reach the dashboard, so we skip it here and let the local-LLM tool
    // return its usual "no model loaded" error at runtime if invoked.
    {
        fb_checkpoint(11, (0, 255, 255));
        let module_resp = MODULE_REQUEST.get_response();
        fb_checkpoint(12, (0, 255, 255));
        let gguf_module = module_resp.and_then(|mods| {
            fb_checkpoint(13, (0, 255, 255));
            mods.modules().iter().copied().find(|f| {
                let path = f.path().to_bytes();
                path.windows(4).any(|w| w == b"gguf")
                    || path.windows(9).any(|w| w == b"model.gguf")
            })
        });
        fb_checkpoint(14, (0, 255, 255));

        if let Some(file) = gguf_module {
            let addr = file.addr() as usize;
            let len = file.size() as usize;
            log::info!(
                "[boot] module: addr={:#x} len={} bytes ({:.2} MB) path={:?}",
                addr,
                len,
                len as f64 / 1024.0 / 1024.0,
                file.path(),
            );
            let bytes: &[u8] =
                unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
            match agent_loop::init_local_model_from_bytes(bytes) {
                Ok(()) => log::info!("[boot] local LLM model loaded from module"),
                Err(e) => log::warn!("[boot] local LLM init failed: {}", e),
            }
        } else {
            log::info!(
                "[boot] no ramdisk module, VFS fallback disabled on real HW — \
                 local LLM tool will return stub error",
            );
        }
    }
    // Phase 3c3 done — yellow
    fb_checkpoint(17, (255, 255, 0));

    // ── Phase 3d: ACPI table discovery ───────────────────────────────
    // Parse ACPI tables for hardware discovery: CPU cores (MADT), power
    // management (FADT), precision timer (HPET), PCIe ECAM (MCFG).
    // Must run after heap init (allocates) but before networking.
    {
        let rsdp_addr = RSDP_REQUEST.get_response().map(|r| r.address() as u64);
        acpi_init::init(rsdp_addr);
    }
    fb_checkpoint(18, (255, 255, 0)); // ACPI done

    // ── Phase 4: Framebuffer ─────────────────────────────────────────
    if let Some(fb_resp) = FRAMEBUFFER_REQUEST.get_response() {
        if let Some(fb) = fb_resp.framebuffers().next() {
            log::info!(
                "[boot] framebuffer: {}x{} pitch={} bpp={}",
                fb.width(),
                fb.height(),
                fb.pitch(),
                fb.bpp(),
            );
            log::info!("[boot] clearing framebuffer...");
            framebuffer::init(framebuffer::LimineFramebufferInfo {
                addr: fb.addr(),
                width: fb.width() as usize,
                height: fb.height() as usize,
                pitch: fb.pitch() as usize,
                bpp: fb.bpp(),
            });
            log::info!("[boot] framebuffer initialized");
        } else {
            log::warn!("[boot] framebuffer response had no framebuffers");
        }
    } else {
        log::warn!("[boot] no framebuffer available, serial-only mode");
    }

    fb_checkpoint(19, (255, 255, 0)); // Framebuffer::init done

    // ── Phase 4b: Boot splash screen + chime ────────────────────────
    // Show the ClaudioOS splash with progress bar and play the boot chime.
    // This runs BEFORE networking, so it's visible even if DHCP/TLS stalls.
    // NOTE: splash clears the screen — the rainbow bars + checkpoint squares
    // disappear here. If we stop seeing them and don't see the ClaudioOS logo,
    // splash is where we hung.
    splash::show_splash(splash::BootStage::Hardware);
    fb_checkpoint(30, (0, 255, 0)); // green: splash returned

    boot_sound::boot_chime();
    fb_checkpoint(31, (0, 255, 0)); // green: boot_chime done

    // ── Phase 4c: Virtual consoles ──────────────────────────────────
    vconsole::init();
    fb_checkpoint(32, (0, 255, 0)); // green: vconsole done

    // ── Phase 5: PCI enumeration + device discovery ──────────────────
    log::info!("[boot] starting PCI enumeration...");
    pci::enumerate();
    log::info!("[boot] PCI enumeration complete");
    fb_checkpoint(33, (0, 255, 0)); // green: PCI enumerated

    // ── Phase 5b: USB (xHCI) host controller + keyboard + mouse ───────
    // TEMPORARILY DISABLED on real hardware — xHCI init hangs on the HP
    // Victus (12th gen Intel). Needs a proper port reset sequence and
    // likely interrupt-driven event ring handling that our QEMU-tuned driver
    // skips. Mouse init depends on USB so is also gated. Re-enable once
    // the xHCI driver has been hardened for real silicon.
    const USB_ON_REAL_HW: bool = false;
    if USB_ON_REAL_HW {
        usb::init();
        mouse::init();
    } else {
        log::warn!("[boot] USB/mouse init skipped (disabled on real hardware)");
    }
    fb_checkpoint(34, (0, 255, 0)); // green: past usb/mouse phase

    // ── Phase 5c: SMP — boot application processors ─────────────────
    // TEMPORARILY DISABLED on real hardware. The 12th-gen Intel hybrid
    // P-core + E-core topology breaks our QEMU-tuned INIT-SIPI-SIPI
    // trampoline (wrong CPU feature expectations on E-cores, LAPIC
    // timing deltas, and the trampoline at 0x8000 collides with UEFI
    // memory on some HP firmware). Boot stays single-core until smp_init
    // has been reworked for real silicon.
    const SMP_ON_REAL_HW: bool = false;
    if SMP_ON_REAL_HW {
        smp_init::init();
    } else {
        log::warn!("[boot] SMP init skipped (disabled on real hardware, single-core boot)");
    }
    fb_checkpoint(36, (0, 255, 0)); // green: past SMP phase

    // ── Phase 5d: Block device registry ──────────────────────────────
    // Walk PCI for mass-storage controllers (AHCI / NVMe), instantiate
    // them, and stash owned handles in `disks::REGISTRY`. Must run after
    // PCI enumeration (Phase 5) but before anything that wants live
    // storage: the swap scanner, the dashboard `df` command, and any
    // VFS mount adapters. Under QEMU with no `-drive` this finds zero
    // controllers and the registry stays empty — all consumers handle
    // that gracefully.
    disks::init();

    // ── Phase 5e: Mount ext4 filesystems from disks ──────────────────
    // Walk the disk registry, parse GPT on each disk, and mount the first
    // Linux-filesystem partition as ext4 at /diskNpN. No-op under QEMU
    // without `-drive`; real hardware exercises the full path.
    storage::mount_disks();

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
    //
    // SAFETY: `new_stack_top` is a valid 16-byte aligned pointer to the top of
    // a freshly allocated 4 MiB heap region. `post_stack_switch` is a valid
    // function pointer. After `mov rsp`, all subsequent pushes and calls use
    // the new stack. The old bootloader stack is abandoned (never freed —
    // it's part of the bootloader's identity mapping and will be reclaimed
    // if we ever remap that memory). This is marked `noreturn` because
    // `post_stack_switch` diverges (enters the executor's halt loop).
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
///
/// This is the time source for ALL network operations: TCP retransmissions,
/// DHCP lease timers, TLS handshake timeouts, and SSE streaming timeouts.
/// The PIT runs at ~18.2 Hz so resolution is ~55ms -- good enough for
/// network protocols but not for sub-millisecond benchmarking.
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
    fb_checkpoint(40, (255, 128, 0)); // orange: main_async / network phase entered

    // ── Step 1: Network stack initialization ──────────────────────────

    // Detect NIC: try VirtIO-net first (QEMU), then Intel NIC (real hardware).
    let virtio_dev = pci::find_device(0x1AF4, 0x1000);
    fb_checkpoint(41, (255, 128, 0)); // orange: virtio probe done

    // Try Intel NIC if no VirtIO-net found.
    let intel_stack = if virtio_dev.is_none() {
        log::info!("[main] no VirtIO-net found, probing for Intel NIC...");
        fb_checkpoint(42, (255, 128, 0)); // orange: starting intel_nic::init_intel_network
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
    fb_checkpoint(43, (255, 128, 0)); // orange: NIC detection phase done

    let nic_dev = virtio_dev;

    match nic_dev {
        None if intel_stack.is_none() => {
            log::warn!("[main] no supported NIC found — skipping networking");
            fb_checkpoint(44, (255, 128, 0)); // orange: no NIC, continuing without net
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

                    // Check for saved session via VFS (/claudio/session.txt), then
                    // QEMU fw_cfg, then compile-time, then OAuth.
                    let mut api_key_buf = alloc::string::String::new();
                    let mut session_cookie_buf = alloc::string::String::new();
                    let mut saved_conv_id = alloc::string::String::new();

                    // Try reading a persisted API-key credential first.
                    if let Some(creds) = claudio_fs::read_credentials() {
                        if let claudio_auth::Credentials::ApiKey(k) = &creds {
                            if !k.is_empty() {
                                api_key_buf = k.clone();
                                log::info!(
                                    "[auth] loaded API key from VFS ({} chars) [REDACTED]",
                                    api_key_buf.len(),
                                );
                            }
                        }
                    }

                    // Try reading session from the VFS first. When ext4-on-AHCI is
                    // mounted this survives reboots; on the current MemFs it only
                    // survives within a session (still useful for hot-reloads).
                    match claudio_fs::read_file("/claudio/session.txt") {
                        Ok(data) => {
                            if let Ok(s) = core::str::from_utf8(&data) {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    let mut lines = trimmed.splitn(2, '\n');
                                    if let Some(cookie) = lines.next() {
                                        session_cookie_buf = alloc::string::String::from(cookie.trim());
                                    }
                                    if let Some(conv) = lines.next() {
                                        saved_conv_id = alloc::string::String::from(conv.trim());
                                    }
                                    log::info!(
                                        "[auth] loaded session from VFS ({} bytes, conv_id={})",
                                        trimmed.len(),
                                        if saved_conv_id.is_empty() { "none" } else { saved_conv_id.as_str() },
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            log::debug!("[auth] no session in VFS: {}", e);
                        }
                    }

                    // Try reading session from QEMU fw_cfg (opt/claudio/session)
                    {
                        // fw_cfg: enumerate entries to find our key
                        // Selector port: 0x510, Data port: 0x511
                        // Entry 0x0000 = signature, 0x0001 = count
                        // File directory at selector 0x0019
                        log::info!("[auth] checking fw_cfg for saved session...");
                        // SAFETY: QEMU fw_cfg I/O ports (0x510 selector, 0x511 data) are safe
                        // to read/write in ring 0. On non-QEMU hardware, these ports are either
                        // unoccupied (reads return 0xFF) or belong to an unrelated device; we
                        // only act on recognized data ("opt/claudio/session" filename match).
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
                                                    log::info!("[oauth] cookies received ({} bytes) [REDACTED]", session_cookies.len());
                                                    for part in session_cookies.split("; ") {
                                                        if part.starts_with("sessionKey=") || part.starts_with("__ssid=") {
                                                            session_cookie_buf = alloc::string::String::from(part);
                                                            log::info!("[oauth] session cookie acquired ({} bytes) [REDACTED]", part.len());
                                                            break;
                                                        }
                                                    }
                                                    if session_cookie_buf.is_empty() {
                                                        // Use all cookies as fallback
                                                        log::warn!("[oauth] no sessionKey or __ssid found, using all cookies");
                                                        session_cookie_buf = session_cookies.clone();
                                                    }

                                                    // Print save marker for host script to capture
                                                    // NOTE: This line intentionally prints the full cookie for the host
                                                    // save-session script to capture via serial. It is the ONLY place
                                                    // where the full cookie is emitted.
                                                    log::info!("[oauth] SAVE_SESSION:{}", session_cookie_buf);
                                                    // Immediately log a redacted confirmation
                                                    log::info!("[oauth] session persisted ({} bytes)", session_cookie_buf.len());

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
                                                            log::info!("[auth] API key acquired ({} chars) [REDACTED]", api_key_buf.len());
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

                    // Persist an API-key credential through claudio-fs/claudio-auth
                    // so the token refresh path and future reboots can pick it up.
                    // Only runs when we actually have an API key (email+code flow).
                    if !api_key.is_empty() {
                        let creds = claudio_auth::Credentials::ApiKey(
                            alloc::string::String::from(api_key),
                        );
                        match claudio_fs::write_credentials(&creds) {
                            Ok(()) => log::info!("[auth] persisted API key credentials to VFS"),
                            Err(e) => log::warn!("[auth] failed to persist credentials: {}", e),
                        }
                    }

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

                            // Persist cookie + conv_id to the VFS so token refresh
                            // and next-boot reuse can pick it up. Format matches the
                            // fw_cfg/VFS loader: "<cookie>\n<conv_id>".
                            let session_blob = alloc::format!("{}\n{}", session_cookie, conv_id);
                            match claudio_fs::write_file("/claudio/session.txt", session_blob.as_bytes()) {
                                Ok(()) => log::info!(
                                    "[auth] persisted session to VFS ({} bytes)",
                                    session_blob.len(),
                                ),
                                Err(e) => log::warn!("[auth] failed to persist session: {}", e),
                            }

                            // Skip the boot test message — it consumes a rate-limit
                            // token and the agent session needs that capacity.
                            log::info!("[claude.ai] skipping boot test (preserving rate limit for agent)");

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

    fb_checkpoint(45, (255, 128, 0)); // orange: match exited, about to fall back
    // ── Fallback: simple keyboard echo loop (no networking) ──────────
    log::info!("[main] falling back to simple keyboard echo loop");

    let fb_w = framebuffer::width();
    let fb_h = framebuffer::height();
    log::info!("[main] setting up terminal layout ({}x{} pixels)", fb_w, fb_h);
    fb_checkpoint(46, (255, 128, 0)); // orange: got fb size

    let mut draw_target = terminal::FramebufferDrawTarget;
    let mut layout = claudio_terminal::Layout::new(fb_w, fb_h);
    fb_checkpoint(47, (255, 128, 0)); // orange: terminal layout created

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
    framebuffer::blit_full(); // flush back buffer to the visible front buffer
    fb_checkpoint(48, (255, 128, 0)); // orange: terminal rendered — screen should now show prompt

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
                framebuffer::blit_full();
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
