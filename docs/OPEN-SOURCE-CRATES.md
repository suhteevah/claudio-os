# ClaudioOS Open-Source Crates

ClaudioOS has published 22 crates as standalone, reusable libraries. Each crate is
`#![no_std]` and can be used independently of ClaudioOS in any bare-metal or
embedded Rust project.

**Repository**: [github.com/suhteevah/baremetal-claude](https://github.com/suhteevah/baremetal-claude)

---

## Published Crates

### Filesystem Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 1 | **ext4-rw** | 3,013 | Read-write ext4 filesystem: superblock, inodes, extent trees, directory entries, bitmap allocation |
| 2 | **btrfs-nostd** | 4,006 | Read-write btrfs: B-tree traversal/modification, chunk mapping, CRC32C checksums, COW semantics |
| 3 | **ntfs-rw** | 3,561 | Read-write NTFS: MFT parsing, data run decoding, B+ tree indexes, $UpCase table, UTF-16LE filenames |

### Hardware Driver Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 4 | **ahci-nostd** | 2,139 | AHCI/SATA driver: HBA register access, port command engine, ATA IDENTIFY, sector read/write |
| 5 | **nvme-nostd** | 2,563 | NVMe driver: admin/IO queue pairs, doorbell registers, PRP scatter-gather, sector I/O |
| 6 | **intel-nic-nostd** | 1,986 | Intel NIC driver: e1000/e1000e (I219-V)/igc (I225-V), DMA descriptor rings, PHY config |
| 7 | **wifi-nostd** | 3,513 | WiFi driver: Intel AX201/AX200, IEEE 802.11, WPA2/WPA3, scanning, association, tx/rx rings |
| 8 | **bluetooth-nostd** | 3,075 | Bluetooth stack: HCI commands/events, L2CAP channels, GAP discovery, GATT services, HID over USB |
| 9 | **usb-storage-nostd** | 1,357 | USB mass storage: Bulk-Only Transport, SCSI command set (INQUIRY, READ/WRITE), sector I/O |
| 10 | **xhci-nostd** | 4,204 | xHCI USB 3.0 host controller: TRB rings, device enumeration, HID keyboard driver |
| 11 | **acpi-nostd** | 2,433 | ACPI table parser: RSDP, RSDT/XSDT, MADT, FADT, MCFG, HPET, shutdown/reboot |
| 12 | **hda-nostd** | 2,631 | Intel HD Audio: CORB/RIRB command protocol, codec discovery, stream setup, PCM playback |
| 13 | **smp-nostd** | 3,391 | SMP support: Local APIC init, AP trampoline boot, per-CPU data, work-stealing scheduler |
| 14 | **gpu-compute-nostd** | 3,392 | NVIDIA GPU compute: MMIO registers, Falcon microcontroller, GPFIFO channels, tensor operations |

### Networking / Security Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 15 | **sshd-pqc** | 4,191 | Post-quantum SSH daemon: ML-KEM-768 + X25519 hybrid KEX, ML-DSA-65 + Ed25519 host keys, RFC 4253/4252/4254 |

### Language / Tool Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 16 | **python-lite** | 2,388 | Minimal Python interpreter: tokenizer, parser, AST evaluator, variables, loops, functions (28 tests) |
| 17 | **js-lite** | 5,229 | Minimal JavaScript evaluator for Cloudflare challenge solving: tokenizer, parser, eval |
| 18 | **rustc-lite** | 142 | Bare-metal Rust compiler using Cranelift code generator backend |
| 19 | **editor-nostd** | 534 | Nano-like text editor: insert/delete, line navigation, save/load (11 tests) |

### Web / Browser Crates

| # | Crate | Lines | Description |
|---|-------|-------|-------------|
| 20 | **wraith-dom** | 2,070 | no_std HTML parser with CSS selector matching and form field detection (32 tests) |
| 21 | **wraith-render** | 1,225 | HTML document to text-mode character grid renderer (12 tests) |
| 22 | **wraith-transport** | 572 | HTTP/HTTPS client over smoltcp TCP/IP stack |

---

## Usage

All crates are designed for `#![no_std]` environments. Add them to your `Cargo.toml`:

```toml
[dependencies]
# Example: using the ext4 filesystem crate
ext4-rw = { git = "https://github.com/suhteevah/baremetal-claude", path = "crates/ext4" }
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
