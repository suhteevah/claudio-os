# ClaudioOS Hardware Drivers

## Supported Hardware Overview

| Driver | Crate / Module | Lines | Status | Hardware |
|--------|----------------|-------|--------|----------|
| AHCI/SATA | `claudio-ahci` | 2,139 | Complete | Any AHCI controller (Intel PCH, AMD) |
| NVMe | `claudio-nvme` | 2,563 | Complete | NVMe 1.4+ SSDs (Samsung, WD, Intel) |
| Intel NIC | `claudio-intel-nic` + `kernel/src/intel_nic.rs` | 1,986 + 454 | Complete + Wired | e1000, e1000e (I219-V), igc (I225-V) |
| WiFi | `claudio-wifi` | 3,513 | Complete | Intel AX201, AX200, 802.11ac/ax |
| Bluetooth | `claudio-bluetooth` | 3,075 | Complete | Bluetooth 5.0+ over USB transport |
| VirtIO-net | `claudio-net` | 3,172 | Complete | QEMU virtio-net-pci (legacy 0.9.5) |
| xHCI USB | `claudio-xhci` + `kernel/src/usb.rs` | 4,204 + 186 | Complete + Wired | USB 3.0 host controllers + HID keyboard |
| USB Mouse | `kernel/src/mouse.rs` | 402 | Complete | USB HID boot protocol mouse |
| USB Storage | `claudio-usb-storage` | 1,357 | Complete | USB mass storage (BOT + SCSI) |
| Touchpad | `kernel/src/touchpad.rs` | 734 | Complete | PS/2 and USB touchpad with gestures |
| HDA Audio | `claudio-hda` | 2,631 | Complete | Intel HD Audio (Realtek, etc.) |
| NVIDIA GPU | `claudio-gpu` | 3,392 | Complete | NVIDIA GPUs (Falcon, FIFO, tensor ops) |
| SMP | `claudio-smp` + `kernel/src/smp_init.rs` | 3,391 + 233 | Complete + Wired | Multi-core x86_64 (APIC, trampoline) |
| ACPI | `claudio-acpi` + `kernel/src/acpi_init.rs` | 2,433 + 523 | Complete + Wired | RSDP/MADT/FADT/MCFG/HPET parsing |
| RTC | `kernel/src/rtc.rs` | 299 | Complete | MC146818 CMOS real-time clock |
| PC Speaker | `kernel/src/boot_sound.rs` | 111 | Complete | PIT channel 2 square wave |
| PS/2 Keyboard | kernel | -- | Complete | PS/2 via IRQ1 (8042 controller) |
| PIT Timer | kernel | -- | Complete | 8253/8254 at 18.2 Hz |
| Serial UART | kernel | -- | Complete | 16550 at 0x3F8, 115200 baud |

---

## WiFi (`crates/wifi/`)

Native WiFi driver for Intel wireless adapters, enabling untethered networking.

### Module Structure

| Module | Purpose |
|--------|---------|
| `driver.rs` | `WifiDriver` with init, scan, connect, disconnect, send/receive |
| `pci.rs` | PCI detection for Intel WiFi adapters (AX201, AX200) |
| `firmware.rs` | Firmware image loading and microcode upload |
| `ieee80211.rs` | IEEE 802.11 frame parsing and construction |
| `scan.rs` | Network scanning: probe requests, beacon parsing, SSID list |
| `tx_rx.rs` | Transmit/receive ring management, DMA buffers |
| `commands.rs` | Firmware command interface, configuration commands |
| `wpa.rs` | WPA2-Personal and WPA3-SAE key derivation and 4-way handshake |

### Supported Adapters

| PCI Device ID | Controller | Common Hardware |
|---------------|-----------|-----------------|
| 0x2723 | Intel Wi-Fi 6 AX200 | Desktop PCIe, laptop M.2 |
| 0xA0F0 | Intel Wi-Fi 6 AX201 | HP Victus, Dell XPS, ThinkPad |

### Usage

```rust
use claudio_wifi::WifiDriver;

let mut wifi = WifiDriver::init(bar0_addr).unwrap();
let networks = wifi.scan().unwrap();
wifi.connect("MyNetwork", "password123").unwrap();
```

---

## Bluetooth (`crates/bluetooth/`)

Full Bluetooth stack over USB transport with HID device support.

### Module Structure

| Module | Purpose |
|--------|---------|
| `driver.rs` | `BluetoothDriver` with init, scan, pair, connect |
| `hci.rs` | Host Controller Interface: commands, events, ACL data |
| `l2cap.rs` | Logical Link Control and Adaptation Protocol: channels, segmentation |
| `gap.rs` | Generic Access Profile: discovery, advertising, connection |
| `gatt.rs` | Generic Attribute Profile: services, characteristics, read/write |
| `hid.rs` | HID over GATT: keyboard, mouse, gamepad input reports |
| `usb_transport.rs` | USB bulk/interrupt endpoint transport for HCI |

### Usage

```rust
use claudio_bluetooth::BluetoothDriver;

let mut bt = BluetoothDriver::init(usb_device).unwrap();
let devices = bt.scan(5_000).unwrap();  // 5 second scan
bt.pair(&devices[0]).unwrap();
```

---

## USB Mass Storage (`crates/usb-storage/`)

USB mass storage driver using the Bulk-Only Transport (BOT) protocol with
SCSI command set for USB thumb drives and external disks.

### Module Structure

| Module | Purpose |
|--------|---------|
| `driver.rs` | `UsbStorageDriver` with init, read_sectors, write_sectors |
| `bot.rs` | Bulk-Only Transport: CBW/CSW packets, bulk endpoint I/O |
| `scsi.rs` | SCSI commands: INQUIRY, READ CAPACITY, READ(10), WRITE(10) |

### Usage

```rust
use claudio_usb_storage::UsbStorageDriver;

let mut drive = UsbStorageDriver::init(usb_device).unwrap();
let mut buf = [0u8; 512];
drive.read_sectors(0, 1, &mut buf).unwrap();
```

---

## Touchpad (`kernel/src/touchpad.rs`)

PS/2 and USB touchpad driver with gesture recognition for laptop use.

### Features

- PS/2 Synaptics and ALPS touchpad detection
- USB HID multi-touch report parsing
- Gesture recognition: tap-to-click, two-finger scroll, two-finger tap (right-click)
- Configurable sensitivity and speed
- Event queue integrated with mouse subsystem

---

## Power Management (`kernel/src/power.rs`)

ACPI-based power management with suspend/resume and battery monitoring.

### Features

- ACPI S3 (suspend to RAM) and S5 (shutdown)
- Battery status via ACPI _BST and _BIF methods
- Battery percentage, charging state, time-to-empty estimation
- Power profiles (performance, balanced, power-saver)
- Idle detection for automatic suspend

### Shell Commands

```
battery          # Show battery status
suspend          # Suspend to RAM (ACPI S3)
shutdown         # Power off (ACPI S5)
```

---

## ACPI Hardware Discovery (`kernel/src/acpi_init.rs`)

The ACPI init module runs early in boot (after heap, before networking) and
populates a global `AcpiInfo` struct used by SMP, power management, and timers.

### Discovery Sequence

1. Find RSDP from UEFI bootloader address (or BIOS memory scan fallback)
2. Parse RSDT/XSDT to enumerate all ACPI tables
3. Parse MADT: extract CPU cores (Local APICs), I/O APICs, interrupt overrides
4. Parse FADT: extract power management registers, parse DSDT for S5 shutdown
5. Parse HPET: enable precision timer, read frequency
6. Parse MCFG: extract PCIe ECAM base addresses

### ACPI Info Provided to Kernel

| Field | Source | Used By |
|-------|--------|---------|
| `cpu_count` | MADT Local APICs | SMP init |
| `local_apic_address` | MADT | SMP init (APIC MMIO base) |
| `io_apics` | MADT | SMP init (interrupt routing) |
| `interrupt_overrides` | MADT | IRQ remapping |
| `fadt_info` | FADT | Shutdown/reboot via PM1a control |
| `hpet_info` | HPET | Precision timing |
| `mcfg_entries` | MCFG | PCIe ECAM config space |

### Power Management

```rust
// ACPI S5 shutdown via PM1a control register:
acpi_init::shutdown();  // Tries ACPI S5, falls back to QEMU port 0x604

// ACPI reboot via reset register:
acpi_init::reboot();    // Tries ACPI reset, falls back to keyboard controller 0xFE
```

---

## SMP Multi-Core Boot (`kernel/src/smp_init.rs`)

The SMP init module boots all application processors discovered via ACPI MADT.

### Init Sequence

1. Read MADT data from `acpi_init` (Local APICs, I/O APICs, APIC base)
2. Verify trampoline page at physical 0x8000 is writable
3. Disable legacy 8259 PIC (mask all IRQs on ports 0x21 and 0xA1)
4. Create `SmpController` with APIC base address
5. Run full SMP init: BSP APIC setup, I/O APIC config, AP boot via SIPI
6. Store controller globally for `spawn_agent_on_core()` / `spawn_agent()`

### Public API

```rust
smp_init::num_cores()                          // Total active cores
smp_init::spawn_agent_on_core(core, name, entry, arg)  // Dispatch to specific core
smp_init::spawn_agent(name, entry, arg)        // Dispatch to least-loaded core
smp_init::apic_eoi()                           // Send EOI from interrupt handlers
```

---

## USB Keyboard + Mouse (`kernel/src/usb.rs`, `kernel/src/mouse.rs`)

### USB Keyboard

The USB subsystem detects xHCI controllers via PCI (class 0x0C/0x03/0x30),
initializes the controller, and bridges USB keyboard events to the existing
PS/2 scancode queue so the dashboard works identically for both.

**Polling Model:** Since MSI-X interrupt routing isn't wired yet, the USB
keyboard is polled periodically from the async executor:

```rust
usb::poll_usb_keyboard();  // Non-blocking, converts HID events to PS/2 scancodes
```

Key press -> `keyboard::push_scancode(scancode)`
Key release -> `keyboard::push_scancode(scancode | 0x80)` (PS/2 Set 1 break code)

### USB Mouse

The mouse module (`kernel/src/mouse.rs`) provides:
- USB HID boot protocol report parsing (3-4 bytes: buttons, dx, dy, scroll)
- Mouse state tracking: position, button state, event queue
- XOR crosshair cursor rendering on the GOP framebuffer
- Global state accessible via `mouse::position()`, `mouse::buttons()`, `mouse::drain_events()`

The cursor uses XOR rendering for visibility on any background color. The
crosshair is 6 pixels in each direction from center.

**Integration status:** The mouse state machine is fully functional. Full
integration awaits xHCI crate support for mouse device enumeration
(HID class=3, subclass=1, protocol=2).

---

## Intel NIC Integration (`kernel/src/intel_nic.rs`)

The Intel NIC module provides a complete smoltcp `Device` adapter for Intel
e1000/e1000e/igc NICs, enabling the same TCP/IP stack used by VirtIO-net.

### Architecture

```
smoltcp Interface
    | Device trait
IntelSmoltcpDevice (intel_nic module)
    | E1000::transmit / E1000::receive
claudio-intel-nic crate
    | MMIO registers + DMA descriptor rings
Intel NIC hardware
```

### Key Features

- PCI detection: scans for Intel vendor 0x8086 against all known device IDs
- BAR0 MMIO mapping: handles both 32-bit and 64-bit BARs
- Page-table walk for virt-to-phys DMA address translation (L4->L3->L2->L1)
- `IntelNetworkStack`: complete smoltcp interface with DHCP
- Automatic NIC selection: kernel tries VirtIO-net first, falls back to Intel NIC

### DHCP Flow

```rust
let stack = intel_nic::init_intel_network(now)?;
// Polls up to 200,000 iterations waiting for DHCP lease
// Returns IntelNetworkStack with IP, gateway, DNS servers
```

---

## SSH Server Wiring (`kernel/src/ssh_server.rs`)

The SSH server module wires the `claudio-sshd` crate to smoltcp TCP and the
dashboard event loop.

### Architecture

- TCP listener on port 22 with 16 KiB RX/TX buffers per connection
- Up to 4 simultaneous SSH sessions
- SSH protocol state machine driven by `poll_ssh_server()` each dashboard loop iteration
- Version exchange, binary packet processing, channel actions
- Echo shell with welcome banner (full pane integration planned)

### Integration

```rust
// During boot:
ssh_server::start_ssh_server(&mut stack, now);

// Each dashboard loop iteration:
ssh_server::poll_ssh_server(&mut stack);
```

---

## Real-Time Clock (`kernel/src/rtc.rs`)

The RTC module reads the MC146818 CMOS real-time clock at boot and combines
it with PIT elapsed ticks to provide a wall clock.

### Features

- Reads CMOS registers via I/O ports 0x70/0x71
- Handles BCD vs binary mode (status register B)
- Handles 12-hour vs 24-hour mode
- Century register support (register 0x32)
- Double-read guard against mid-update races
- Unix timestamp conversion (accurate 1970-2099)
- PIT-corrected wall clock: `rtc::wall_clock()` returns current DateTime

### Public API

```rust
rtc::init();                     // Read RTC at boot, store timestamp
rtc::wall_clock() -> DateTime    // Current time (boot RTC + PIT elapsed)
rtc::wall_clock_formatted()      // "YYYY-MM-DD HH:MM:SS"
rtc::uptime_seconds()            // Seconds since boot
rtc::boot_timestamp()            // Unix timestamp of boot time
```

---

## AHCI/SATA (`crates/ahci/`)

AHCI (Advanced Host Controller Interface) provides a standard register-level
interface to SATA drives. ClaudioOS detects AHCI controllers via PCI class
0x01/subclass 0x06.

### Module Structure

| Module | Purpose |
|--------|---------|
| `hba.rs` | HBA (Host Bus Adapter) registers: global regs, port regs, volatile MMIO |
| `port.rs` | Per-port state machine: idle, BSY/DRQ wait, command slot management |
| `command.rs` | Command table construction: CFIS (H2D Register FIS), PRDT entries |
| `identify.rs` | ATA IDENTIFY DEVICE parsing: model, serial, capacity, features |
| `driver.rs` | High-level `AhciController` + `AhciDisk` with sector read/write |

### Usage

```rust
use claudio_ahci::AhciController;

let abar: u64 = /* PCI BAR5 */;
let mut ctrl = AhciController::init(abar);
for disk in ctrl.disks() {
    let mut buf = [0u8; 512];
    disk.read_sectors(0, 1, &mut buf).unwrap();
}
```

---

## NVMe (`crates/nvme/`)

NVMe provides high-performance access to solid-state storage via PCIe memory-mapped
I/O. Queue pairs (submission + completion) with doorbell registers enable concurrent
sector I/O.

### Module Structure

| Module | Purpose |
|--------|---------|
| `registers.rs` | Controller registers: CAP, VS, CC, CSTS, AQA, ASQ, ACQ, doorbells |
| `queue.rs` | Submission/Completion queue pair: ring buffer, phase bit tracking |
| `admin.rs` | Admin commands: Identify Controller, Identify Namespace, Create I/O Queue |
| `io.rs` | I/O commands: Read, Write, Flush with scatter-gather PRP lists |
| `driver.rs` | `NvmeController` + `NvmeDisk` with sector-level API |

### Usage

```rust
use claudio_nvme::NvmeController;

let mut ctrl = NvmeController::init(bar0_addr).unwrap();
let mut disk = ctrl.namespace(1).unwrap();
let mut buf = [0u8; 512];
disk.read_sectors(0, 1, &mut buf).unwrap();
```

---

## Intel NIC (`crates/intel-nic/`)

Supports the Intel e1000 family of Ethernet controllers for real hardware
(the VirtIO-net driver is used in QEMU).

### Supported Controllers

| PCI Device ID | Controller | Common Hardware |
|---------------|-----------|-----------------|
| 0x100E | e1000 (82540EM) | QEMU fallback, older servers |
| 0x15B8 | e1000e (I219-V) | Desktop Intel LAN (i9-11900K) |
| 0x15F3 | igc (I225-V) | 2.5GbE desktop LAN |

### Module Structure

| Module | Purpose |
|--------|---------|
| `regs.rs` | Register definitions: CTRL, STATUS, RCTL, TCTL, RDBAL/H, TDBAL/H |
| `rx.rs` | Receive descriptor ring: DMA buffers, head/tail management |
| `tx.rs` | Transmit descriptor ring: DMA buffers, RS/EOP flags |
| `phy.rs` | PHY configuration: MDIO register access, link speed/duplex |
| `driver.rs` | `IntelNic` with init, send_packet, recv_packet, link_status |

---

## xHCI USB 3.0 (`crates/xhci/`)

xHCI (eXtensible Host Controller Interface) provides USB 1.1/2.0/3.0 support
through a unified register interface. ClaudioOS uses it primarily for USB
keyboard and mouse input on real hardware (replacing PS/2).

### Module Structure

| Module | Purpose |
|--------|---------|
| `registers.rs` | Capability, Operational, Runtime, Doorbell register sets |
| `trb.rs` | Transfer Request Block types: Normal, Setup, Data, Status, Event, Link |
| `ring.rs` | TRB ring management: enqueue, dequeue, cycle bit, link TRBs |
| `context.rs` | Device/Endpoint context structures for slot assignment |
| `device.rs` | USB device enumeration: address, configure, interface/endpoint discovery |
| `hid.rs` | HID keyboard driver: report descriptor parsing, scancode translation |
| `driver.rs` | `XhciController` with init, poll, and keyboard event retrieval |

---

## HDA Audio (`crates/hda/`)

Intel High Definition Audio (HDA) provides audio playback through a codec
discovery + command/response protocol.

### Module Structure

| Module | Purpose |
|--------|---------|
| `registers.rs` | HDA controller registers: GCAP, GCTL, CORBBASE, RIRBBASE, stream regs |
| `corb.rs` | Command Outbound Ring Buffer: send verb commands to codecs |
| `rirb.rs` | Response Inbound Ring Buffer: receive codec responses |
| `codec.rs` | Codec discovery: widget tree walk, pin config, DAC/ADC routing |
| `stream.rs` | Stream descriptor setup: BDL (Buffer Descriptor List), format, DMA |
| `driver.rs` | `HdaController` with init, discover_codecs, play_pcm |

---

## NVIDIA GPU (`crates/gpu/`)

Bare-metal NVIDIA GPU driver for compute workloads (not display). Based on
reverse-engineering from the nouveau project and envytools.

### Module Structure

| Module | Purpose |
|--------|---------|
| `pci_config.rs` | PCI vendor 0x10DE detection, BAR0/BAR1 mapping, GPU family detect |
| `mmio.rs` | MMIO register blocks: NV_PMC, PFIFO, PFB, PGRAPH, PTIMER |
| `memory.rs` | GPU VRAM management, GPU page tables, host-to-GPU DMA mapping |
| `falcon.rs` | Falcon microcontroller: firmware upload, PMU/SEC2/GSP-RM boot |
| `fifo.rs` | GPFIFO channels: push buffers, runlists, doorbell submission |
| `compute.rs` | Compute class setup: shader program load, grid/block dispatch |
| `tensor.rs` | Tensor operations: matmul, softmax, layernorm, GELU activation |
| `driver.rs` | `GpuDevice` high-level API: init, query capabilities, dispatch compute |

---

## SMP Multi-Core (`crates/smp/`)

Symmetric Multi-Processing support enables running agent sessions across
multiple CPU cores.

### Module Structure

| Module | Purpose |
|--------|---------|
| `apic.rs` | Local APIC initialization, IPI (Inter-Processor Interrupt) sending |
| `trampoline.rs` | AP (Application Processor) boot: real-mode trampoline code at 0x8000 |
| `percpu.rs` | Per-CPU data structures: current task, local run queue, CPU ID |
| `scheduler.rs` | Work-stealing scheduler: per-core run queues, idle steal from neighbors |
| `driver.rs` | `SmpManager` with init, boot_aps, spawn_on_core |

### AP Boot Sequence

```
BSP (Boot Strap Processor):
  1. Parse ACPI MADT for AP APIC IDs
  2. Copy trampoline code to 0x8000 (below 1 MiB)
  3. Send INIT IPI to each AP
  4. Wait 10ms
  5. Send STARTUP IPI with vector 0x08 (-> 0x8000)
  6. AP wakes in real mode at 0x8000

AP (Application Processor):
  1. Execute trampoline: real mode -> protected mode -> long mode
  2. Load GDT, IDT, page tables (shared with BSP)
  3. Init local APIC
  4. Allocate per-CPU stack + data
  5. Enter scheduler idle loop
```

---

## ACPI (`crates/acpi/`)

ACPI table parsing provides hardware discovery and power management.

### Supported Tables

| Table | Purpose |
|-------|---------|
| RSDP | Root System Description Pointer -- entry point to ACPI tables |
| RSDT/XSDT | Root/Extended System Description Table -- table of table pointers |
| MADT | Multiple APIC Description Table -- APIC IDs, I/O APICs, interrupt overrides |
| FADT | Fixed ACPI Description Table -- PM timer, power management registers |
| MCFG | Memory-mapped Configuration Space -- PCIe ECAM base address |
| HPET | High Precision Event Timer -- nanosecond-resolution timer |

### Module Structure

| Module | Purpose |
|--------|---------|
| `rsdp.rs` | RSDP detection: scan EBDA + 0xE0000-0xFFFFF, validate checksum |
| `tables.rs` | Generic SDT header parsing, RSDT/XSDT traversal |
| `madt.rs` | MADT parsing: local APICs, I/O APICs, ISO, NMI entries |
| `fadt.rs` | FADT parsing: PM1a/PM1b control blocks, century register, boot flags |
| `mcfg.rs` | MCFG parsing: ECAM base, bus range for PCIe config space |
| `hpet.rs` | HPET parsing: base address, comparator count, min tick |
| `driver.rs` | `AcpiTables` with init, shutdown, reboot |
