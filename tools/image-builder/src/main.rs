//! Disk image builder for ClaudioOS.
//!
//! Takes the compiled kernel binary and creates bootable UEFI and BIOS disk images
//! using the `bootloader` crate's image builder.
//!
//! Usage:
//!   cargo run --package claudio-image-builder -- <path-to-kernel-binary>
//!
//! Example:
//!   cargo run --package claudio-image-builder -- target/x86_64-unknown-none/debug/claudio-os

use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let kernel_path = args
        .next()
        .expect("usage: claudio-image-builder <kernel-binary-path> [--ramdisk <gguf-path>]");

    // Optional --ramdisk <path>: any file baked into the image and exposed
    // to the kernel via BootInfo::ramdisk_addr/ramdisk_len. We use this to
    // ship a GGUF model for init_local_model_from_bytes() at boot.
    let mut ramdisk: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        if a == "--ramdisk" {
            ramdisk = Some(PathBuf::from(
                args.next().expect("--ramdisk needs a path"),
            ));
        } else {
            eprintln!("warn: unknown arg {:?}", a);
        }
    }

    let kernel_path = Path::new(&kernel_path);
    if !kernel_path.exists() {
        eprintln!("error: kernel binary not found at {:?}", kernel_path);
        eprintln!("hint: run `cargo build` first to compile the kernel");
        std::process::exit(1);
    }

    if let Some(rd) = ramdisk.as_ref() {
        if !rd.exists() {
            eprintln!("error: ramdisk file not found at {:?}", rd);
            std::process::exit(1);
        }
        let sz = std::fs::metadata(rd).map(|m| m.len()).unwrap_or(0);
        println!("[image] ramdisk: {:?} ({} bytes, {:.2} MB)", rd, sz, sz as f64 / 1024.0 / 1024.0);
    }

    let out_dir = kernel_path
        .parent()
        .unwrap_or(Path::new("."));

    // Create UEFI disk image
    let uefi_path = out_dir.join("claudio-os-uefi.img");
    println!("[image] creating UEFI disk image at {:?}", uefi_path);
    let mut uefi_builder = bootloader::UefiBoot::new(kernel_path);
    if let Some(rd) = ramdisk.as_ref() {
        uefi_builder.set_ramdisk(rd);
    }
    uefi_builder
        .create_disk_image(&uefi_path)
        .expect("failed to create UEFI disk image");
    println!("[image] UEFI image: {:?} ({} bytes)", uefi_path, std::fs::metadata(&uefi_path).map(|m| m.len()).unwrap_or(0));

    // BIOS image disabled — bootloader's BIOS stages don't build on the
    // Windows host (build.rs panics in the cargo-install bootstrap of
    // bootloader-x86_64-bios-*). The kernel only ships UEFI per CLAUDE.md.

    println!();
    println!("[image] done! To boot in QEMU:");
    println!();
    println!("  UEFI boot (requires OVMF):");
    println!("    qemu-system-x86_64 \\");
    println!("      -bios /usr/share/OVMF/OVMF_CODE.fd \\");
    println!("      -drive format=raw,file={} \\", uefi_path.display());
    println!("      -serial stdio -m 512M");
}
