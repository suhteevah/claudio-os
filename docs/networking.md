# Networking Stack

The networking subsystem provides the full path from Ethernet frames to HTTPS requests.
It lives in `crates/net/`.

**Status:** Phase 2 -- driver and protocol code is written, not yet activated in
the boot sequence.

---

## Table of Contents

- [Architecture](#architecture)
- [VirtIO-net Driver](#virtio-net-driver)
- [smoltcp Integration](#smoltcp-integration)
- [TLS Strategy](#tls-strategy)
- [HTTP/1.1 Client](#http11-client)
- [SSE Parser](#sse-parser)

---

## Architecture

```
+---------------------------+
|  HTTP client (http.rs)    |  HttpRequest / HttpResponse / SseEvent
+---------------------------+
            |
            v raw bytes
+---------------------------+
|  TLS stream (tls.rs)      |  TlsStream wrapping TCP socket
+---------------------------+
            |
            v encrypted TLS records
+---------------------------+
|  TCP socket (smoltcp)     |  smoltcp::socket::tcp::Socket
+---------------------------+
            |
            v TCP segments
+---------------------------+
|  IP layer (smoltcp)       |  smoltcp::iface::Interface
+---------------------------+
            |
            v IP packets
+---------------------------+
|  Ethernet (smoltcp)       |  smoltcp::phy::Device trait
+---------------------------+
            |
            v Ethernet frames
+---------------------------+
|  NIC driver (nic.rs)      |  VirtioNet implementing NicDriver trait
+---------------------------+
            |
            v VirtIO virtqueues (DMA)
+---------------------------+
|  Hardware / QEMU          |
+---------------------------+
```

**Source files:**

| File | Purpose |
|------|---------|
| `crates/net/src/lib.rs` | NicDriver trait, InitError, high-level init function |
| `crates/net/src/nic.rs` | VirtIO-net driver (legacy 0.9.5 PCI transport) |
| `crates/net/src/stack.rs` | smoltcp Interface + DHCP + DNS wrapper |
| `crates/net/src/tls.rs` | TLS stream (embedded-tls or rustls) |
| `crates/net/src/dns.rs` | DNS resolver using smoltcp |
| `crates/net/src/http.rs` | HTTP/1.1 request/response, chunked encoding, SSE |

---

## VirtIO-net Driver

**Source:** `crates/net/src/nic.rs`

### VirtIO Legacy (0.9.5) Specification

The driver uses the legacy VirtIO interface, which communicates with the device through
PCI I/O port registers. This is simpler than the modern (1.0+) MMIO interface and works
well with QEMU's default `virtio-net-pci` device.

### PCI I/O Register Map

All registers are relative to the BAR0 I/O base address from PCI config space:

| Offset | Size | R/W | Register |
|--------|------|-----|----------|
| 0x00 | 4 | R | Device Features |
| 0x04 | 4 | R+W | Guest Features |
| 0x08 | 4 | R+W | Queue Address (PFN) |
| 0x0C | 2 | R | Queue Size |
| 0x0E | 2 | R+W | Queue Select |
| 0x10 | 2 | R+W | Queue Notify |
| 0x12 | 1 | R+W | Device Status |
| 0x13 | 1 | R | ISR Status |
| 0x14 | 6 | R | MAC Address (device-specific config) |

### Initialization Sequence

```
Step 1: RESET
    Write 0 to DEVICE_STATUS
    (resets the device to a known state)

Step 2: ACKNOWLEDGE
    Write VIRTIO_STATUS_ACK (0x01) to DEVICE_STATUS
    (guest OS has noticed the device)

Step 3: DRIVER
    Write ACK | DRIVER (0x03) to DEVICE_STATUS
    (guest OS knows how to drive this device)

Step 4: FEATURE NEGOTIATION
    Read DEVICE_FEATURES (what the device supports)
    Write GUEST_FEATURES (what we want)
    - VIRTIO_NET_F_MAC (bit 5): read MAC from config space
    - VIRTIO_NET_F_STATUS (bit 16): link status available
    - NOT requesting MRG_RXBUF (keeps header at 10 bytes)
    Write ACK | DRIVER | FEATURES_OK (0x0B) to DEVICE_STATUS

Step 5: READ MAC ADDRESS
    Read 6 bytes from VIRTIO_MAC_BASE (offset 0x14)

Step 6: SET UP RX QUEUE (index 0)
    Write 0 to QUEUE_SELECT
    Read QUEUE_SIZE (typically 256)
    Allocate legacy virtqueue (contiguous memory layout)
    Write PFN (physical page frame number) to QUEUE_ADDR

Step 7: SET UP TX QUEUE (index 1)
    Write 1 to QUEUE_SELECT
    Read QUEUE_SIZE
    Allocate legacy virtqueue
    Write PFN to QUEUE_ADDR

Step 8: POPULATE RX QUEUE
    For each descriptor slot:
      - Allocate a 2048-byte buffer
      - Set descriptor: addr=phys_addr, len=2048, flags=WRITE
      - Push descriptor index to available ring

Step 9: DRIVER_OK
    Write ACK | DRIVER | FEATURES_OK | DRIVER_OK (0x0F)
    (device is now live and can receive/transmit)
```

### Virtqueue Memory Layout (Legacy)

Legacy VirtIO requires the descriptor table, available ring, and used ring to be in
a specific contiguous layout:

```
+------------------------------------------------------+
| Descriptor Table                                      |
| 16 bytes * queue_size                                 |
| (16-byte aligned)                                     |
|                                                       |
| Each descriptor:                                      |
|   addr:  u64  -- physical address of buffer           |
|   len:   u32  -- buffer length                        |
|   flags: u16  -- NEXT(1), WRITE(2), INDIRECT(4)      |
|   next:  u16  -- index of next descriptor in chain    |
+------------------------------------------------------+
| Available Ring                                        |
| flags: u16                                            |
| idx:   u16  -- next entry the driver will write       |
| ring:  [u16; queue_size]  -- descriptor head indices  |
| used_event: u16                                       |
+------------------------------------------------------+
| Padding to next 4096-byte page boundary               |
+------------------------------------------------------+
| Used Ring                                             |
| flags: u16                                            |
| idx:   u16  -- next entry the device will write       |
| ring:  [(id: u32, len: u32); queue_size]              |
| avail_event: u16                                      |
+------------------------------------------------------+
```

The entire region is allocated as a single page-aligned block. The physical page
frame number (PFN = phys_addr >> 12) of the base is written to the `QUEUE_ADDR`
register.

### TX Flow

```
1. Reclaim completed TX descriptors (pop from used ring, return to free list)
2. Allocate a descriptor from the free list
3. Build buffer: VirtIO-net header (10 bytes, all zeros) + Ethernet frame
4. Set descriptor: addr=phys, len=hdr+frame, flags=0 (device reads)
5. Push descriptor index to available ring
6. Memory barrier (Release fence)
7. Increment available ring idx
8. Notify device: write 1 (TX queue index) to QUEUE_NOTIFY port
```

### RX Flow

```
1. Check used ring: if used.idx != last_used_idx, a frame arrived
2. Read used ring entry: (descriptor_index, bytes_written)
3. Strip VirtIO-net header (first 10 bytes)
4. Copy Ethernet frame to caller's buffer
5. Recycle descriptor:
   - Reset descriptor: addr=phys, len=2048, flags=WRITE
   - Push back to available ring
   - Notify device: write 0 (RX queue index) to QUEUE_NOTIFY port
```

### VirtIO-net Header

Every frame is prepended with a 10-byte header (when `MRG_RXBUF` is not negotiated):

```c
struct virtio_net_hdr {
    uint8_t  flags;        // NEEDS_CSUM, etc.
    uint8_t  gso_type;     // NONE, TCPV4, etc.
    uint16_t hdr_len;      // Ethernet + IP + TCP header length
    uint16_t gso_size;     // Bytes to use for MSS
    uint16_t csum_start;   // Offset to start checksumming
    uint16_t csum_offset;  // Offset to place checksum
};
```

ClaudioOS sets all fields to zero (no offload features used).

### Address Translation

The driver needs physical addresses for DMA but works with virtual addresses
internally. Translation uses the bootloader's physical memory offset:

```
physical_address = virtual_address - phys_mem_offset
```

---

## smoltcp Integration

**Source:** `crates/net/src/stack.rs`

The `NetworkStack` struct wraps:
- A `VirtioNet` driver instance
- A `smoltcp::iface::Interface` configured for Ethernet
- DHCP and DNS socket handles

### Device Trait Implementation

The smoltcp `Device` trait is implemented for the VirtIO-net driver, bridging the
`NicDriver::transmit()/receive()` API to smoltcp's `Token`-based frame delivery.

### DHCP

DHCP is handled by smoltcp's built-in DHCP client socket. The high-level `init()`
function polls the stack in a loop until `has_ip` becomes true or a timeout is
reached (100,000 poll iterations).

QEMU's SLIRP networking provides:
- IP: 10.0.2.15 (typical)
- Gateway: 10.0.2.2
- DNS: 10.0.2.3

### DNS Resolution

**Source:** `crates/net/src/dns.rs`

DNS resolution uses smoltcp's DNS socket, querying the DHCP-provided DNS server.
The resolver is synchronous (poll loop) and returns the first A record.

---

## TLS Strategy

**Source:** `crates/net/src/tls.rs`

### Challenge

TLS in a `#![no_std]` bare-metal environment is one of the hardest parts of the
project. Options evaluated:

| Option | Pros | Cons |
|--------|------|------|
| `embedded-tls` 0.17 | Designed for no_std, small | LLVM codegen crashes on x86_64-unknown-none |
| `rustls` (no_std fork) | Battle-tested, modern TLS | Large, requires ring or aws-lc-rs |
| Custom minimal TLS 1.3 | Total control | Enormous effort, security risk |
| `bearssl` via FFI | Small C library | Requires C toolchain in build |

The current plan is `embedded-tls` with workarounds for the LLVM crashes (possibly
using specific optimization levels or codegen flags). If that fails, a `rustls`
no_std port or `bearssl` FFI binding will be explored.

### TlsStream API

The `TlsStream` type wraps a TCP socket handle and provides read/write methods
that encrypt/decrypt through the TLS library:

```rust
pub struct TlsStream {
    // TCP socket handle
    // TLS session state
    // Read/write buffers
}

impl TlsStream {
    pub fn handshake(stack, tcp, hostname, now) -> Result<Self, TlsError>;
    pub fn send(stack, data, now) -> Result<usize, TlsError>;
    pub fn receive(stack, buf, now) -> Result<usize, TlsError>;
}
```

---

## HTTP/1.1 Client

**Source:** `crates/net/src/http.rs`

A minimal HTTP/1.1 implementation with no external dependencies beyond `alloc`.

### Request Building

```rust
let req = HttpRequest::post("api.anthropic.com", "/v1/messages", body)
    .header("Content-Type", "application/json")
    .header("x-api-key", api_key)
    .header("anthropic-version", "2023-06-01");

let raw_bytes: Vec<u8> = req.to_bytes();
```

The `to_bytes()` method produces a complete HTTP/1.1 request:

```
POST /v1/messages HTTP/1.1\r\n
Host: api.anthropic.com\r\n
Content-Length: 1234\r\n
Content-Type: application/json\r\n
x-api-key: sk-...\r\n
anthropic-version: 2023-06-01\r\n
\r\n
{"model":"claude-sonnet-4-20250514",...}
```

### Response Parsing

```rust
let resp = HttpResponse::parse(raw_data)?;
// resp.status: u16 (200, 404, etc.)
// resp.reason: String ("OK", "Not Found")
// resp.headers: Vec<(String, String)>
// resp.body: Vec<u8>
```

The parser:
1. Scans for `\r\n\r\n` to find the header/body boundary
2. Parses the status line (`HTTP/1.1 200 OK`)
3. Parses each header line (`Name: value`)
4. Reads the body based on `Content-Length` or takes everything after headers

### Chunked Transfer Encoding

The `decode_chunked()` function handles HTTP chunked transfer encoding:

```
Chunked format:
  <hex-size>\r\n
  <data>\r\n
  <hex-size>\r\n
  <data>\r\n
  0\r\n
  \r\n
```

Each chunk's size is parsed as hex. A chunk size of 0 terminates the stream.

### Convenience Helpers

Two helper functions build pre-configured requests:

- **`anthropic_messages_request(api_key, body_json)`**: Builds a POST to
  `/v1/messages` with proper headers (`Content-Type`, `x-api-key`,
  `anthropic-version`, `Accept: text/event-stream`)

- **`oauth_device_code_request(client_id)`**: Builds a POST to
  `/oauth/device/code` for the OAuth device flow

---

## SSE Parser

**Source:** `crates/net/src/http.rs` (bottom section)

Server-Sent Events (SSE) are used for streaming responses from the Anthropic
Messages API.

### SSE Format

```
event: content_block_delta\n
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}\n
\n
event: message_stop\n
data: {"type":"message_stop"}\n
\n
```

### Parser

`parse_sse_events(data: &[u8])` processes a chunk of SSE stream data:

1. Split on `\n`
2. For each line:
   - `event: <value>` sets the current event type
   - `data: <value>` appends to the current data (multiple `data:` lines are
     joined with newlines)
   - Empty line terminates an event and pushes it to the result vec
3. Returns `(Vec<SseEvent>, bytes_consumed)`

The `bytes_consumed` return value tells the caller how much of the input buffer
was fully processed, so incomplete events at the end are not lost.

### SseEvent Type

```rust
pub struct SseEvent {
    pub event: String,   // e.g., "content_block_delta", "message_stop"
    pub data: String,    // JSON payload
}
```

### Test Coverage

The HTTP module includes unit tests (runnable on the host via `cargo test`):
- Request serialization
- Response parsing (complete and incomplete)
- Chunked encoding decoding
- SSE event parsing
- Case-insensitive header lookup
