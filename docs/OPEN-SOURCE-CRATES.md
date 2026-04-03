# ClaudioOS Open-Source Crates

ClaudioOS has 52 workspace crates across 35 published GitHub repos. Each crate is
`#![no_std]` and can be used independently of ClaudioOS in any bare-metal or
embedded Rust project.

**Repository**: [github.com/suhteevah/claudio-os](https://github.com/suhteevah/claudio-os)

---

## Published Crates

### Filesystem Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 1 | **ext4-rw** | 3,013 | Read-write ext4 filesystem: superblock, inodes, extent trees, directory entries, bitmap allocation |
| 2 | **btrfs-nostd** | 4,006 | Read-write btrfs: B-tree traversal/modification, chunk mapping, CRC32C checksums, COW semantics |
| 3 | **ntfs-rw** | 3,561 | Read-write NTFS: MFT parsing, data run decoding, B+ tree indexes, $UpCase table, UTF-16LE filenames |
| 4 | **vfs-nostd** | 2,871 | Virtual filesystem layer: mount table, GPT/MBR partition detection, POSIX file API |

### Hardware Driver Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 5 | **ahci-nostd** | 2,139 | AHCI/SATA driver: HBA register access, port command engine, ATA IDENTIFY, sector read/write |
| 6 | **nvme-nostd** | 2,563 | NVMe driver: admin/IO queue pairs, doorbell registers, PRP scatter-gather, sector I/O |
| 7 | **intel-nic-nostd** | 1,986 | Intel NIC driver: e1000/e1000e (I219-V)/igc (I225-V), DMA descriptor rings, PHY config |
| 8 | **wifi-nostd** | 3,513 | WiFi driver: Intel AX201/AX200, IEEE 802.11, WPA2/WPA3, scanning, association, tx/rx rings |
| 9 | **bluetooth-nostd** | 3,075 | Bluetooth stack: HCI commands/events, L2CAP channels, GAP discovery, GATT services, HID over USB |
| 10 | **usb-storage-nostd** | 1,357 | USB mass storage: Bulk-Only Transport, SCSI command set (INQUIRY, READ/WRITE), sector I/O |
| 11 | **xhci-nostd** | 4,204 | xHCI USB 3.0 host controller: TRB rings, device enumeration, HID keyboard driver |
| 12 | **acpi-nostd** | 2,433 | ACPI table parser: RSDP, RSDT/XSDT, MADT, FADT, MCFG, HPET, shutdown/reboot |
| 13 | **hda-nostd** | 2,631 | Intel HD Audio: CORB/RIRB command protocol, codec discovery, stream setup, PCM playback |
| 14 | **smp-nostd** | 3,391 | SMP support: Local APIC init, AP trampoline boot, per-CPU data, work-stealing scheduler |
| 15 | **gpu-compute-nostd** | 3,392 | NVIDIA GPU compute: MMIO registers, Falcon microcontroller, GPFIFO channels, tensor operations |
| 16 | **elf-loader-nostd** | 1,213 | ELF binary loader: ELF64 parsing, section/segment mapping, relocation, entry point execution |
| 17 | **linux-compat-nostd** | 4,090 | Linux syscall translation layer: syscall dispatch, /proc emulation, signal handling, mmap stubs |

### Networking / Security Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 44 | **sshd-pqc** | 4,191 | Post-quantum SSH daemon: ML-KEM-768 + X25519 hybrid KEX, ML-DSA-65 + Ed25519 host keys, RFC 4253/4252/4254 |
| 45 | **net-nostd** | 3,172 | Network stack: VirtIO-net driver, smoltcp TCP/IP, TLS 1.3, HTTP/SSE client |

### Language / Tool Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 20 | **python-lite** | 2,388 | Minimal Python interpreter: tokenizer, parser, AST evaluator, variables, loops, functions (28 tests) |
| 21 | **js-lite** | 5,229 | Minimal JavaScript evaluator for Cloudflare challenge solving: tokenizer, parser, eval |
| 22 | **rustc-lite** | 142 | Bare-metal Rust compiler using Cranelift code generator backend |
| 23 | **go-lite** | -- | Go interpreter: goroutines, channels, interfaces, structs, slices, maps |
| 24 | **cpp-lite** | -- | C++ interpreter: classes, templates, RAII, virtual dispatch, STL subset |
| 25 | **lua-lite** | -- | Lua interpreter: tables, metatables, coroutines, closures, pattern matching |
| 26 | **ts-lite** | -- | TypeScript interpreter: type checking, interfaces, generics, enums, modules |
| 27 | **jvm-lite** | -- | JVM bytecode interpreter: class loading, garbage collector, threads, exceptions |
| 28 | **wasm-runtime** | -- | WebAssembly runtime: module validation, execution engine, WASI subset |
| 29 | **cc-lite** | -- | C interpreter: pointers, structs, unions, malloc/free, preprocessor macros |
| 30 | **asm-x86** | -- | x86-64 assembler: Intel syntax, labels, relocations, ELF output |
| 31 | **editor-nostd** | 534 | Nano-like text editor: insert/delete, line navigation, save/load (11 tests) |
| 32 | **shell-nostd** | 2,884 | AI-native shell: 45+ builtins, pipes, environment variables, natural language mode |
| 33 | **terminal-nostd** | 2,930 | Framebuffer terminal: split panes, ANSI/VTE parsing, font rendering, scroll |
| 34 | **agent-nostd** | 501 | Agent session lifecycle: tool loop (20 rounds), conversation state management |

### Windows Compatibility Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 35 | **pe-loader-nostd** | 1,497 | PE/COFF binary loader: parsing, section mapping, relocation, import resolution |
| 36 | **win32-nostd** | 10,458 | Win32 API layer: kernel32, user32, gdi32, DirectWrite, Direct2D, WASAPI, XInput, WIC |
| 37 | **dotnet-clr-nostd** | 5,179 | .NET Common Language Runtime: PE/CLI loader, IL interpreter, garbage collector, BCL |
| 38 | **winrt-nostd** | 1,676 | Windows Runtime API projection: activation factories, metadata parsing, async patterns |

### Graphics Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 39 | **vulkan-nostd** | 3,811 | Vulkan 1.3 GPU driver: instance, device, swapchain, command buffers, pipeline, shaders |
| 40 | **dxvk-bridge-nostd** | 2,039 | DirectX 9/10/11 to Vulkan translation layer (DXVK-style), D3D device/context emulation |

### Web / Browser Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 41 | **wraith-dom** | 2,070 | no_std HTML parser with CSS selector matching and form field detection (32 tests) |
| 42 | **wraith-render** | 1,225 | HTML document to text-mode character grid renderer (12 tests) |
| 43 | **wraith-transport** | 572 | HTTP/HTTPS client over smoltcp TCP/IP stack |

---

## Usage

All crates are designed for `#![no_std]` environments. Add them to your `Cargo.toml`:

```toml
[dependencies]
# Example: using the ext4 filesystem crate
ext4-rw = { git = "https://github.com/suhteevah/claudio-os", path = "crates/ext4" }
```

Or if published to crates.io:

```toml
[dependencies]
ext4-rw = "0.1"
```

### Common Pattern

Every hardware driver and filesystem crate follows the same pattern:

1. Implement a trait for your hardware backend (`BlockDevice`, `NicBackend`, etc.)
2. Call `init()` or `mount()` with hardware addresses from PCI enumeration
3. Use the high-level API (read/write sectors, send/receive packets, etc.)

### Example: ext4

```rust
#![no_std]
extern crate alloc;

use ext4_rw::{Ext4Fs, BlockDevice};

struct MyDisk { /* your storage backend */ }

impl BlockDevice for MyDisk {
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<(), MyError> {
        // Read 512 bytes from disk at logical block address
    }
    fn write_sector(&mut self, lba: u64, buf: &[u8]) -> Result<(), MyError> {
        // Write 512 bytes to disk
    }
    fn sector_size(&self) -> u32 { 512 }
    fn total_sectors(&self) -> u64 { /* disk size / 512 */ }
}

let fs = Ext4Fs::mount(my_disk).expect("mount failed");
let data = fs.read_file(b"/etc/hostname").expect("read failed");
```

### Example: NVMe

```rust
use nvme_nostd::NvmeController;

// BAR0 physical address from PCI enumeration
let mut ctrl = NvmeController::init(bar0_addr).expect("nvme init");
let mut disk = ctrl.namespace(1).expect("ns1");

let mut buf = [0u8; 4096];
disk.read_sectors(0, 8, &mut buf).expect("read");
```

### Example: Python Interpreter

```rust
use python_lite::Interpreter;

let mut interp = Interpreter::new();
let output = interp.run("
x = 42
for i in range(5):
    x = x + i
print(x)
");
assert_eq!(output.trim(), "52");
```

---

## Testing

Most crates include unit tests that run on the host (not bare-metal):

```bash
# Run tests for a specific crate
cargo test -p python-lite
cargo test -p ext4-rw
cargo test -p wraith-dom

# Run all tests
cargo test --workspace
```

Test counts by crate:
- `python-lite`: 28 tests
- `wraith-dom`: 32 tests
- `wraith-render`: 12 tests
- `editor-nostd`: 11 tests

---

## Contribution Guidelines

### Code Style

- All crates are `#![no_std]` with `extern crate alloc` where heap is needed
- Use `log` crate macros for all logging
- Use `spin::Mutex` for shared state (no std Mutex)
- Volatile MMIO for all hardware register access
- Comprehensive doc comments on public types and functions

### Adding a New Crate

1. Create the crate directory under `crates/`
2. Add it to `workspace.members` in the root `Cargo.toml`
3. Use `workspace.package` for version, edition, authors, license
4. Ensure `#![no_std]` compiles cleanly
5. Add unit tests for all logic that can run on the host
6. Document the crate in this file and `docs/ARCHITECTURE.md`

### License

- **ClaudioOS kernel and integration code**: AGPL-3.0-or-later
- **Published standalone crates**: Intended for MIT + Apache-2.0 dual license
  (check individual crate `Cargo.toml` for current license)

Copyright (c) Ridge Cell Repair LLC
