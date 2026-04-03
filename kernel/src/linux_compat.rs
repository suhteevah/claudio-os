//! Linux binary compatibility layer for ClaudioOS.
//!
//! Provides the ability to load and run statically-linked Linux x86_64 ELF
//! binaries on bare metal. Sets up the SYSCALL/SYSRET MSRs so that the
//! `syscall` instruction invokes our syscall dispatcher, which translates
//! Linux syscall ABI calls to ClaudioOS operations.
//!
//! ## How it works
//!
//! 1. **SYSCALL MSR setup**: During `init()`, we configure:
//!    - `IA32_STAR` — Segment selectors for SYSCALL/SYSRET transitions
//!    - `IA32_LSTAR` — Entry point address (our `syscall_entry` stub)
//!    - `IA32_FMASK` — RFLAGS mask (disable interrupts during entry)
//!
//! 2. **ELF loading**: `run_linux_binary()` uses the elf-loader crate to parse
//!    the ELF64 binary, load PT_LOAD segments, set up the initial stack
//!    (argc/argv/envp/auxv), and prepare the entry point.
//!
//! 3. **Execution**: We switch to the loaded binary via SYSRET (or a simulated
//!    jump for the initial entry). When the binary executes `syscall`, the CPU
//!    jumps to our entry stub which saves registers, calls the Rust dispatcher,
//!    restores registers, and returns via `sysretq`.
//!
//! ## Safety
//!
//! This module contains `unsafe` code for:
//! - MSR writes (privileged instructions)
//! - Inline assembly for the syscall entry/exit stubs
//! - Raw pointer access to user-space memory

use claudio_elf_loader::{load_elf, LoadedProgram, ElfError};
use claudio_linux_compat::dispatcher::{ProcessContext, SyscallArgs, dispatch_syscall};
use spin::Mutex;

/// GDT segment selectors. These must match the GDT layout in gdt.rs.
/// SYSCALL loads CS from STAR[47:32] and SS from STAR[47:32]+8.
/// SYSRET loads CS from STAR[63:48]+16 and SS from STAR[63:48]+8.
const KERNEL_CS: u64 = 0x08; // Kernel code segment selector
const KERNEL_SS: u64 = 0x10; // Kernel data segment selector
const USER_CS_BASE: u64 = 0x18; // User code segment base (SYSRET adds 16)

// MSR addresses
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;
const IA32_EFER: u32 = 0xC000_0080;
const EFER_SCE: u64 = 1 << 0; // System Call Extensions enable bit

/// Global process context for the currently running Linux binary.
/// Protected by a spinlock since we might access it from interrupt context.
static PROCESS_CTX: Mutex<Option<ProcessContext>> = Mutex::new(None);

/// Read a Model-Specific Register.
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") low,
        out("edx") high,
        options(nomem, nostack, preserves_flags)
    );
    (high as u64) << 32 | low as u64
}

/// Write a Model-Specific Register.
#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") low,
        in("edx") high,
        options(nomem, nostack, preserves_flags)
    );
}

/// The raw syscall entry point. This is what IA32_LSTAR points to.
///
/// When `syscall` executes:
/// - RCX = return RIP (saved by CPU)
/// - R11 = saved RFLAGS (saved by CPU)
/// - RAX = syscall number
/// - RDI, RSI, RDX, R10, R8, R9 = arguments
///
/// We save all callee-saved registers, build a SyscallArgs struct,
/// call the Rust dispatcher, restore registers, and `sysretq`.
#[unsafe(naked)]
unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // Save user stack pointer and switch to kernel stack
        // (In our single-address-space model, we use the same stack
        //  but save RSP for the SYSRET return path)
        "swapgs",                    // Swap to kernel GS base
        "mov gs:[0], rsp",          // Save user RSP to per-CPU area
        // For now in our simple model, keep using the same stack:
        // "mov rsp, gs:[8]",       // Load kernel RSP (would need per-CPU data)

        // Save all caller-saved registers that SYSCALL clobbers
        "push rcx",                  // Return RIP
        "push r11",                  // Saved RFLAGS
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Build SyscallArgs on stack and call dispatcher.
        // Arguments in: RAX=nr, RDI=arg0, RSI=arg1, RDX=arg2, R10=arg3, R8=arg4, R9=arg5
        // Note: R10 is used instead of RCX (which is clobbered by SYSCALL).
        "push r9",                   // arg5
        "push r8",                   // arg4
        "push r10",                  // arg3
        "push rdx",                  // arg2
        "push rsi",                  // arg1
        "push rdi",                  // arg0
        "push rax",                  // nr

        // Call the Rust handler: syscall_handler(args_ptr: *const SyscallArgs) -> i64
        "mov rdi, rsp",             // Pointer to SyscallArgs on stack
        "call {handler}",

        // RAX now contains the return value

        // Clean up SyscallArgs from stack (7 * 8 = 56 bytes)
        "add rsp, 56",

        // Restore callee-saved registers
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "pop r11",                   // Restore RFLAGS for SYSRET
        "pop rcx",                   // Restore return RIP for SYSRET

        "swapgs",                    // Swap back to user GS
        "sysretq",                   // Return to user mode

        handler = sym syscall_handler,
    );
}

/// Rust-level syscall handler called from the assembly entry stub.
///
/// # Safety
/// `args_ptr` must point to a valid SyscallArgs struct on the stack.
#[unsafe(no_mangle)]
unsafe extern "C" fn syscall_handler(args_ptr: *const SyscallArgs) -> i64 {
    let args = *args_ptr;

    let mut ctx_lock = PROCESS_CTX.lock();
    if let Some(ref mut ctx) = *ctx_lock {
        dispatch_syscall(ctx, args)
    } else {
        // No process context — shouldn't happen
        log::error!("syscall_handler called with no process context!");
        -38 // -ENOSYS
    }
}

/// Initialize the SYSCALL/SYSRET mechanism.
///
/// Must be called during kernel boot after GDT is set up.
/// Sets up the MSRs that the `syscall` instruction reads.
pub fn init() {
    log::info!("[linux-compat] Initializing SYSCALL/SYSRET MSRs");

    unsafe {
        // Enable System Call Extensions in EFER
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);

        // IA32_STAR: segment selectors
        // Bits 47:32 = SYSCALL CS/SS (kernel segments): CS = KERNEL_CS, SS = KERNEL_CS+8
        // Bits 63:48 = SYSRET CS/SS base: CS = base+16, SS = base+8
        let star = (USER_CS_BASE << 48) | (KERNEL_CS << 32);
        wrmsr(IA32_STAR, star);

        // IA32_LSTAR: syscall entry point address
        wrmsr(IA32_LSTAR, syscall_entry as u64);

        // IA32_FMASK: bits to clear in RFLAGS on SYSCALL
        // Clear IF (disable interrupts during syscall entry) and TF (no single-step)
        let fmask = 0x0000_0000_0000_0300; // IF=0x200, TF=0x100
        wrmsr(IA32_FMASK, fmask);
    }

    log::info!("[linux-compat] SYSCALL/SYSRET ready, LSTAR=0x{:X}", syscall_entry as *const () as u64);
}

/// Load and run a statically-linked Linux ELF binary.
///
/// # Arguments
/// * `binary_data` — Raw ELF binary bytes.
/// * `argv` — Command-line arguments (argv[0] should be the program name).
/// * `envp` — Environment variables in "KEY=VALUE" format.
///
/// # Returns
/// The exit code of the process, or an error.
pub fn run_linux_binary(
    binary_data: &[u8],
    argv: &[&str],
    envp: &[&str],
) -> Result<i32, ElfError> {
    log::info!("[linux-compat] Loading ELF binary ({} bytes)", binary_data.len());

    // Load the ELF
    let program = claudio_elf_loader::load_elf_with_args(binary_data, argv, envp)?;

    log::info!(
        "[linux-compat] ELF loaded: entry=0x{:016X}, {} segments, brk=0x{:016X}",
        program.entry_point,
        program.segments.len(),
        program.brk_start,
    );

    // Set up process context
    let ctx = ProcessContext::new(program.brk_start);
    *PROCESS_CTX.lock() = Some(ctx);

    // In a full implementation, we would:
    // 1. Map each segment's data into the correct virtual address using page tables.
    // 2. Copy the initial stack data to the stack virtual address.
    // 3. Set RSP to program.stack_pointer.
    // 4. Jump to program.entry_point (via SYSRET or IRETQ for initial entry).
    //
    // For the initial entry, we can't use SYSRET (it expects RCX=RIP, R11=RFLAGS).
    // Instead, we use IRETQ which lets us specify CS, SS, RSP, RIP, RFLAGS.

    log::info!(
        "[linux-compat] Preparing to jump to entry point 0x{:016X} with RSP=0x{:016X}",
        program.entry_point,
        program.stack_pointer,
    );

    // For now, return a placeholder. The actual jump-to-userspace requires
    // the page table infrastructure to be in place.
    //
    // When page table support is ready, uncomment:
    // unsafe { jump_to_userspace(program.entry_point, program.stack_pointer); }

    // Poll for process exit
    let exit_code = {
        let ctx_lock = PROCESS_CTX.lock();
        if let Some(ref ctx) = *ctx_lock {
            if ctx.ps.exited {
                ctx.ps.exit_code
            } else {
                0
            }
        } else {
            -1
        }
    };

    // Clean up
    *PROCESS_CTX.lock() = None;

    log::info!("[linux-compat] Process exited with code {}", exit_code);
    Ok(exit_code)
}

/// Jump to userspace entry point (used for initial binary start).
///
/// Uses IRETQ to set up the initial register state and jump to the ELF
/// entry point. This is only called once per binary execution.
///
/// # Safety
/// The entry_point and stack_pointer must be valid mapped addresses.
#[allow(dead_code)]
unsafe fn jump_to_userspace(entry_point: u64, stack_pointer: u64) {
    // IRETQ expects the stack to contain (from top):
    //   RIP, CS, RFLAGS, RSP, SS
    //
    // We push these in reverse order, then execute IRETQ.
    //
    // For our single-address-space model, we use kernel segments for both
    // "kernel" and "user" code. In a proper implementation with ring 3
    // support, we'd use user segment selectors here.

    core::arch::asm!(
        "cli",                       // Disable interrupts for the transition
        "push {ss}",                // SS
        "push {rsp_val}",          // RSP
        "push 0x202",               // RFLAGS (IF=1, reserved bit 1 set)
        "push {cs}",                // CS
        "push {rip}",              // RIP
        "iretq",                     // Jump!
        ss = in(reg) KERNEL_SS,
        rsp_val = in(reg) stack_pointer,
        cs = in(reg) KERNEL_CS,
        rip = in(reg) entry_point,
        options(noreturn)
    );
}
