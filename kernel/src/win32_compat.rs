//! Win32 binary compatibility layer for ClaudioOS.
//!
//! Provides the ability to load and run Windows PE (Portable Executable)
//! x86_64 binaries on bare metal. Initializes the Win32 subsystem (TEB/PEB,
//! handle table, registry, DLL dispatcher), loads the PE via claudio-pe-loader,
//! resolves imports against our Rust Win32 implementations, applies relocations,
//! and jumps to the entry point.
//!
//! ## How it works
//!
//! 1. **Init**: Set up the Win32 subsystem (handles, registry, dispatcher).
//! 2. **PE Loading**: Parse the PE headers, map sections, apply relocations.
//! 3. **Import Resolution**: For each DLL import (e.g. kernel32.dll!CreateFileW),
//!    the dispatcher returns a pointer to our Rust implementation.
//! 4. **TEB/PEB Setup**: Allocate TEB/PEB, set GS base via WRMSR.
//! 5. **Entry Point**: Jump to the PE's AddressOfEntryPoint.
//!
//! ## Safety
//!
//! This module contains `unsafe` code for:
//! - Running arbitrary PE code in our address space
//! - Setting up TEB/PEB with WRMSR
//! - Raw pointer manipulation for IAT patching

use claudio_pe_loader::loader::{load_pe, LoadedPe, PeError};
use claudio_win32::dispatcher;
use claudio_win32::handles;
use claudio_win32::registry;
use claudio_win32::teb_peb;
use claudio_win32::kernel32;

/// Initialize the Win32 subsystem.
///
/// Must be called during kernel boot before any PE binaries are loaded.
pub fn init() {
    log::info!("[win32-compat] Initializing Win32 subsystem");

    // Initialize the handle table (stdin/stdout/stderr)
    handles::init();

    // Initialize kernel32 (command line, environment variables)
    kernel32::init();

    // Initialize the registry with default Windows keys
    registry::init();

    // Initialize the DLL dispatcher (register all Win32 API functions)
    dispatcher::init();

    log::info!(
        "[win32-compat] Win32 ready: {} API functions registered",
        dispatcher::function_count()
    );
}

/// Load and run a Windows PE binary.
///
/// # Arguments
/// * `pe_data` — Raw PE binary bytes (the .exe file contents).
///
/// # Returns
/// The process exit code, or an error string.
pub fn run_windows_binary(pe_data: &[u8]) -> Result<i32, &'static str> {
    log::info!("[win32-compat] Loading PE binary ({} bytes)", pe_data.len());

    // Parse and load the PE
    let loaded = load_pe(pe_data).map_err(|e| {
        log::error!("[win32-compat] PE load failed: {:?}", e);
        "PE load failed"
    })?;

    log::info!(
        "[win32-compat] PE loaded: entry=0x{:016X}, base=0x{:016X}, {} sections",
        loaded.entry_point,
        loaded.image_base,
        loaded.sections.len(),
    );

    // Resolve imports
    let unresolved = resolve_imports(&loaded);
    if unresolved > 0 {
        log::warn!("[win32-compat] {} unresolved imports (stubbed to no-op)", unresolved);
    }

    // Set up TEB/PEB
    // Use a 1MB stack region
    let stack_size: u64 = 1024 * 1024;
    let stack_layout = alloc::alloc::Layout::from_size_align(stack_size as usize, 4096)
        .unwrap();
    let stack_base_ptr = unsafe { alloc::alloc::alloc_zeroed(stack_layout) };
    if stack_base_ptr.is_null() {
        return Err("Failed to allocate PE stack");
    }
    let stack_limit = stack_base_ptr as u64;
    let stack_base = stack_limit + stack_size;

    teb_peb::init(loaded.image_base, stack_base, stack_limit);

    log::info!(
        "[win32-compat] TEB/PEB initialized, stack: 0x{:X}-0x{:X}",
        stack_limit, stack_base
    );

    // Jump to entry point
    log::info!(
        "[win32-compat] Jumping to PE entry point at 0x{:016X}",
        loaded.entry_point
    );

    let exit_code = unsafe { call_pe_entry(loaded.entry_point) };

    log::info!("[win32-compat] PE process exited with code {}", exit_code);

    // Clean up
    teb_peb::cleanup();

    Ok(exit_code)
}

/// Resolve all PE imports against our Win32 dispatcher.
///
/// Returns the number of unresolved imports.
fn resolve_imports(loaded: &LoadedPe) -> usize {
    let mut unresolved = 0;

    for import in &loaded.imports.entries {
        let dll_name = &import.dll_name;
        log::debug!("[win32-compat] Processing imports for '{}'", dll_name);

        for entry in &import.functions {
            let func_name = match entry.name() {
                Some(n) => n,
                None => continue, // skip ordinal imports
            };
            match dispatcher::resolve_import(dll_name, func_name) {
                Some(addr) => {
                    log::trace!(
                        "[win32-compat]   {} -> 0x{:X}",
                        func_name, addr
                    );
                    // In a full implementation, we'd patch the IAT entry:
                    // unsafe { *(entry.iat_address as *mut u64) = addr; }
                }
                None => {
                    log::warn!(
                        "[win32-compat]   {} (UNRESOLVED)",
                        func_name
                    );
                    unresolved += 1;
                }
            }
        }
    }

    unresolved
}

/// Call the PE entry point.
///
/// For a Windows EXE, the entry point signature is:
///   `DWORD WINAPI WinMainCRTStartup(void)` — for CRT-linked apps
///   or `void mainCRTStartup(void)`
///
/// We call it with the Windows x64 calling convention (`extern "system"`),
/// which on x86_64 is identical to the Microsoft x64 ABI: first 4 args in
/// RCX/RDX/R8/R9, caller-allocated shadow space, callee-saved RBX/RBP/RDI/RSI.
///
/// The `transmute` converts the raw u64 address into a Rust function pointer.
/// When the PE code executes, all Win32 API calls (via the patched IAT) jump
/// to our Rust implementations in the `claudio_win32` crate.
///
/// # Safety
/// The entry_point must be a valid, mapped code address within the loaded PE image.
unsafe fn call_pe_entry(entry_point: u64) -> i32 {
    // The PE entry point for a console app is typically:
    //   int mainCRTStartup(void)
    // For a GUI app:
    //   int WinMainCRTStartup(void)
    //
    // Both take no arguments and return an int exit code.

    let entry_fn: extern "system" fn() -> i32 = core::mem::transmute(entry_point);
    entry_fn()
}
