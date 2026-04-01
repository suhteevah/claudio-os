# Networking Stack

The networking subsystem provides the full path from Ethernet frames to HTTPS
requests. It lives in `crates/net/`.

**Status:** COMPLETE -- All networking subsystems are active and tested. The VirtIO-net
driver, smoltcp stack, TLS 1.3, and HTTP/SSE client are integrated into the kernel
boot sequence and have been used to make live API calls to `api.anthropic.com`.

---

## Table of Contents

- [Architecture](#architecture)
- [VirtIO-net Driver](#virtio-net-driver)
- [smoltcp Integration](#smoltcp-integration)
- [DNS Resolution](#dns-resolution)
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
            v raw bytes over TLS
+---------------------------+
|  TLS stream (tls.rs)      |  TlsStream wrapping TCP socket handle
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
|  Hardware / QEMU SLIRP    |  10.0.2.x NAT, outbound HTTPS works
+---------------------------+
```

### Source Files

| File | Size | Purpose |
|------|------|---------|
| `crates/net/src/lib.rs` | ~210 lines | `NicDriver` trait, `PciDeviceInfo`, high-level `init()` with DHCP poll loop |
| `crates/net/src/nic.rs` | ~757 lines | VirtIO-net legacy PCI driver (virtqueue alloc, DMA, page table walk) |
| `crates/net/src/stack.rs` | ~248 lines | smoltcp `Device` adapter, `NetworkStack`, DHCP event processing |
| `crates/net/src/dns.rs` | ~125 lines | DNS resolver using smoltcp's DNS socket |
| `crates/net/src/tls.rs` | ~326 lines | TCP connect/send/recv helpers, `TlsStream` (handshake stubbed) |
| `crates/net/src/http.rs` | ~564 lines | HTTP/1.1 request/response, chunked encoding, SSE parser, tests |

### NicDriver Trait

All NIC drivers implement this trait so the smoltcp device adapter is generic:

```rust
pub trait NicDriver {
    fn transmit(&mut self, frame: &[u8]) -> Result<(), NicError>;
    fn receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NicError>;
    fn mac_address(&self) -> [u8; 6];
}
```

---

## VirtIO-net Driver

**Source:** `crates/net/src/nic.rs`

### VirtIO Legacy (0.9.5) Specification

The driver uses the legacy VirtIO interface via PCI I/O port registers. This is
simpler than the modern (1.0+) MMIO interface and compatible with QEMU's default
`virtio-net-pci` device.

### PCI I/O Register Map (relative to BAR0 I/O base)

| Offset | Size | R/W | Name | Purpose |
|--------|------|-----|------|---------|
| `0x00` | 4 | R | `DEVICE_FEATURES` | Feature bits the device supports |
| `0x04` | 4 | R+W | `GUEST_FEATURES` | Feature bits the driver accepts |
| `0x08` | 4 | R+W | `QUEUE_ADDR` | Physical page frame number of virtqueue |
| `0x0C` | 2 | R | `QUEUE_SIZE` | Max descriptors in selected queue |
| `0x0E` | 2 | R+W | `QUEUE_SELECT` | Select which queue to configure (0=RX, 1=TX) |
| `0x10` | 2 | R+W | `QUEUE_NOTIFY` | Kick the device to process a queue |
| `0x12` | 1 | R+W | `DEVICE_STATUS` | Driver lifecycle status bits |
| `0x13` | 1 | R | `ISR_STATUS` | Interrupt status (read clears) |
| `0x14` | 6 | R | `MAC_BASE` | Device MAC address (device-specific config) |

### Device Status Bits

```
ACKNOWLEDGE   = 0x01   Guest has found the device
DRIVER        = 0x02   Guest knows how to drive this device
FEATURES_OK   = 0x08   Feature negotiation complete
DRIVER_OK     = 0x04   Driver is ready, device is live
FAILED        = 0x80   Something went wrong
```

### Full Initialization Sequence

```
Step 1: RESET
    Write 0 to DEVICE_STATUS register.
    Resets all device state.

Step 2: ACKNOWLEDGE (0x01)
    Write ACK to DEVICE_STATUS.
    "I see you, device."

Step 3: DRIVER (0x03)
    Write ACK | DRIVER.
    "I know what kind of device you are."

Step 4: FEATURE NEGOTIATION
    Read DEVICE_FEATURES:
      Device advertises what it supports (MAC, STATUS, MRG_RXBUF, etc.)
    Write GUEST_FEATURES:
      We request: VIRTIO_NET_F_MAC (bit 5) + VIRTIO_NET_F_STATUS (bit 16)
      We do NOT request MRG_RXBUF -- this keeps the virtio-net header
      at exactly 10 bytes (vs 12 with mergeable buffers).
    Write ACK | DRIVER | FEATURES_OK (0x0B).
    Legacy devices may not require FEATURES_OK, but it's harmless.

Step 5: READ MAC ADDRESS
    Read 6 bytes from MAC_BASE (offset 0x14-0x19).
    If VIRTIO_NET_F_MAC not advertised, use fallback 52:54:00:12:34:56.

Step 6: SET UP RX QUEUE (index 0)
    Write 0 to QUEUE_SELECT.
    Read QUEUE_SIZE (typically 256 for QEMU).
    Allocate contiguous legacy virtqueue memory layout (see below).
    Write physical PFN (page frame number) to QUEUE_ADDR.

Step 7: SET UP TX QUEUE (index 1)
    Write 1 to QUEUE_SELECT.
    Read QUEUE_SIZE.
    Allocate + configure as above.
    Write PFN to QUEUE_ADDR.

Step 8: POPULATE RX QUEUE
    For each free descriptor slot (up to queue_size):
      Allocate a 2048-byte DMA buffer (Box<[u8; 2048]>).
      Set descriptor: addr=phys_addr, len=2048, flags=WRITE (device writes here).
      Push descriptor index into the available ring.
    Log how many descriptors were populated.

Step 9: DRIVER_OK (0x0F)
    Write ACK | DRIVER | FEATURES_OK | DRIVER_OK.
    Device is now live. RX buffers are waiting, TX queue is ready.
    Read back status to verify acceptance.
```

### Virtqueue Memory Layout (Legacy Spec)

Legacy VirtIO requires the descriptor table, available ring, and used ring in a
specific contiguous layout. This is allocated as a single page-aligned block:

```
+------------------------------------------------------+  <-- base (page-aligned)
| Descriptor Table                                      |
| 16 bytes per descriptor * queue_size                  |
| (16-byte aligned)                                     |
|                                                       |
| struct VirtqDesc {                                    |
|     addr:  u64   // physical address of data buffer   |
|     len:   u32   // buffer length in bytes             |
|     flags: u16   // NEXT=1, WRITE=2, INDIRECT=4       |
|     next:  u16   // next descriptor index in chain     |
| }                                                     |
+------------------------------------------------------+
| Available Ring                                        |
| flags:      u16                                       |
| idx:        u16  // next entry driver will write       |
| ring:       [u16; queue_size]  // descriptor indices   |
| used_event: u16  // (event suppression, legacy compat) |
+------------------------------------------------------+
| Padding to next 4096-byte page boundary               |
+------------------------------------------------------+  <-- page-aligned
| Used Ring                                             |
| flags:       u16                                      |
| idx:         u16  // next entry device will write      |
| ring:        [(id: u32, len: u32); queue_size]         |
| avail_event: u16  // (event suppression)               |
+------------------------------------------------------+

Physical Page Frame Number = phys_addr_of_base >> 12
Written to QUEUE_ADDR register.
```

Free descriptors are chained via the `next` field. The driver maintains `free_head`
and `num_free` to manage the free list.

### TX Flow

```
1. Reclaim completed TX descriptors:
   While used ring has new entries (used.idx != last_used_idx):
     Read used ring entry (descriptor index, bytes written)
     Return descriptor to free list
2. Allocate a descriptor from free list
3. Build buffer in descriptor's pre-allocated 2048-byte Box:
   Bytes 0-9:  VirtIO-net header (all zeros = no offload)
   Bytes 10+:  Ethernet frame (dest MAC + src MAC + ethertype + payload)
4. Set descriptor: addr=virt_to_phys(buf), len=10+frame_len, flags=0 (device reads)
5. Push descriptor index to available ring
6. Memory barrier (Release fence) -- ensure descriptor writes visible before idx update
7. Increment available ring idx
8. Notify device: write 1 (TX queue index) to QUEUE_NOTIFY
```

### RX Flow

```
1. Check used ring: if used.idx == last_used_idx, no frame available -> return None
2. Read used ring entry: (descriptor_index, total_bytes_written)
3. If total <= 10 (header only): recycle descriptor, return None (runt frame)
4. frame_len = total - 10 (strip VirtIO-net header)
5. Copy frame from descriptor buffer[10..10+frame_len] to caller's buffer
6. Recycle the descriptor:
   a. Reset: addr=phys, len=2048, flags=WRITE
   b. Push to available ring
   c. Notify device: write 0 (RX queue index) to QUEUE_NOTIFY
7. Return Ok(Some(frame_len))
```

### Address Translation (virt_to_phys)

The VirtIO device does DMA using **physical addresses**, but the driver works with
virtual addresses. For addresses in the physical memory offset mapping region, the
simple formula `phys = virt - phys_mem_offset` works. However, heap-allocated
buffers (at `0x4444_4444_0000+`) are mapped separately.

The `VirtQueue::virt_to_phys()` method performs a full 4-level page table walk:

```
1. Read CR3 -> L4 page table physical address
2. L4[virt.p4_index()] -> L3 physical address (check for 1 GiB huge page)
3. L3[virt.p3_index()] -> L2 physical address (check for 2 MiB huge page)
4. L2[virt.p2_index()] -> L1 physical address
5. L1[virt.p1_index()] -> frame physical address
6. Return frame_phys + page_offset (12-bit offset within 4 KiB page)
```

Each level is accessed through the phys_mem_offset mapping. Panics if any entry
is unused (indicates a bug in the heap mapping).

### VirtIO-net Header

Every frame is prepended with a 10-byte header (when `MRG_RXBUF` is not negotiated):

```c
struct virtio_net_hdr {       // 10 bytes
    uint8_t  flags;           // NEEDS_CSUM, etc.
    uint8_t  gso_type;        // NONE, TCPV4, UDP, etc.
    uint16_t hdr_len;         // Ethernet + IP + TCP header len
    uint16_t gso_size;        // MSS for segmentation
    uint16_t csum_start;      // Offset to start checksumming
    uint16_t csum_offset;     // Offset to place checksum
};
```

ClaudioOS sets all fields to zero on TX (no hardware offload). On RX, the header
is stripped and only the Ethernet frame is returned.

---

## smoltcp Integration

**Source:** `crates/net/src/stack.rs`

### SmoltcpDevice

The `SmoltcpDevice` struct wraps `VirtioNet` and implements smoltcp's `Device` trait:

```
Device::receive(timestamp):
    Call nic.receive() to get one Ethernet frame
    Return (SmoltcpRxToken, SmoltcpTxToken) if frame available

Device::transmit(timestamp):
    Return SmoltcpTxToken (always available)

SmoltcpRxToken::consume(f):
    Pass received frame bytes to closure f

SmoltcpTxToken::consume(len, f):
    Allocate Vec of len bytes
    Let closure f fill the buffer
    Call nic.transmit(&buf) to send the frame

Device::capabilities():
    medium: Ethernet
    MTU: 1514 (standard Ethernet)
    max_burst_size: 1
```

### NetworkStack

The high-level `NetworkStack` struct owns the smoltcp `Interface`, `SocketSet`,
and `SmoltcpDevice`. It provides:

- `poll(timestamp)`: Drive the stack forward. Processes incoming frames, DHCP
  state transitions, TCP/UDP state machines. Returns true if socket state changed.
- `process_dhcp()`: Check DHCPv4 socket for configuration events. On `Configured`:
  apply IP address, gateway, DNS servers to the interface.
- `ipv4_addr()`: Get current IPv4 address (from DHCP).
- `nic_mut()`: Access underlying NIC for interrupt acknowledgment.

### DHCP

DHCP is handled by smoltcp's built-in DHCPv4 socket. The high-level `init()`
function in `crates/net/src/lib.rs` polls the stack in a tight loop until `has_ip`
becomes true or a timeout is reached (100,000 iterations).

QEMU SLIRP networking provides:
- IP: 10.0.2.15 (typical)
- Gateway: 10.0.2.2
- DNS: 10.0.2.3

---

## DNS Resolution

**Source:** `crates/net/src/dns.rs`

DNS resolution uses smoltcp's built-in DNS socket. The `resolve()` function is
synchronous (drives the network stack poll loop internally):

```
1. Check that DNS servers are configured (from DHCP)
2. Create a smoltcp DNS socket with the DHCP-provided DNS server addresses
3. Start an A record query for the hostname
4. Poll the network stack in a loop (up to 10,000 iterations)
5. Check for query result each iteration:
   - Pending: continue polling
   - Ok(addresses): find first IPv4 address, return it
   - Failed: return DnsError::NotFound
6. Clean up: remove DNS socket from socket set
7. Return Ipv4Address on success
```

This is a blocking operation -- in Phase 3+, it will be wrapped in an async
interface that yields between poll iterations.

---

## TLS 1.3

**Source:** `crates/net/src/tls.rs`

### Implementation

TLS is implemented using `embedded-tls` 0.17 with the AES-128-GCM-SHA256 cipher suite.
This was one of the hardest parts of the project due to several bare-metal constraints.

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| `embedded-tls` over `rustls` | Designed for no_std, smaller footprint |
| Custom target `x86_64-claudio.json` | Enables SSE+AES-NI at LLVM level for crypto |
| 16-byte aligned buffers | AES-NI requires aligned data |
| `-cpu Haswell` in QEMU | Provides AES-NI hardware instructions |
| Certificate verification disabled | Bare-metal has no CA root store (security tradeoff) |

### Problems Solved

1. **LLVM codegen crash**: The default `x86_64-unknown-none` target uses soft-float
   (-soft-float feature). The `embedded-tls` crypto code uses SIMD intrinsics that
   conflict. Solution: custom target with `+sse,+sse2,+aes,+pclmulqdq`.

2. **AES-NI alignment**: AES-NI instructions (AESENC, AESDEC) require 16-byte
   aligned memory. TLS buffers are explicitly aligned in the allocation.

3. **memchr AVX2 crash**: The `memchr` crate auto-detects AVX2 at runtime and uses
   it, but the default QEMU CPU doesn't support it. `-cpu Haswell` provides the
   necessary instruction set.

### TCP Helpers

The module provides raw TCP connection helpers used by TLS and HTTP:

- **`tcp_connect(stack, ip, port, local_port, now)`**: Creates a smoltcp TCP socket
  with 8 KiB RX/TX buffers, initiates connection, polls until connected or timeout.
- **`tcp_send(stack, handle, data, now)`**: Sends data slice over connected TCP socket,
  polling until all bytes are sent. Includes send queue drain (waits for ACK).
- **`tcp_recv(stack, handle, buf, now)`**: Receives data into buffer, returns byte count.
  Returns `Ok(0)` on graceful close. Detects CloseWait state for EOF.
- **`tcp_close(stack, handle)`**: Sends TCP close, removes socket from set.

### TlsStream

The `TlsStream` type wraps a TCP socket handle with TLS 1.3 encryption:

- **`connect(stack, ip, port, local_port, hostname, now)`**: TCP connect + TLS handshake in one call
- **`handshake(stack, tcp_handle, hostname, now)`**: TLS handshake over existing TCP connection
- **`send(stack, data, now)`**: Encrypt and send data through TLS record layer
- **`recv(stack, buf, now)`**: Receive and decrypt data from TLS record layer

The handshake negotiates TLS 1.3 with AES-128-GCM-SHA256. SNI is set from the
hostname parameter. Certificate verification is skipped (no CA root store available
in bare-metal environment).

---

## HTTP/1.1 Client

**Source:** `crates/net/src/http.rs`

A minimal HTTP/1.1 implementation with zero dependencies beyond `alloc`. No chunked
transfer encoding support on send, no redirects, no keep-alive management, no
connection pooling -- just enough to POST JSON to the Anthropic API and parse the
response.

### Request Building

```rust
let req = HttpRequest::post("api.anthropic.com", "/v1/messages", body)
    .header("Content-Type", "application/json")
    .header("x-api-key", api_key)
    .header("anthropic-version", "2023-06-01")
    .header("Accept", "text/event-stream");

let raw_bytes: Vec<u8> = req.to_bytes();
```

`to_bytes()` produces a complete HTTP/1.1 request:

```
POST /v1/messages HTTP/1.1\r\n
Host: api.anthropic.com\r\n
Content-Length: 1234\r\n
Content-Type: application/json\r\n
x-api-key: sk-...\r\n
anthropic-version: 2023-06-01\r\n
Accept: text/event-stream\r\n
\r\n
{"model":"claude-sonnet-4-20250514",...}
```

### Response Parsing

The parser operates in stages:

1. **Find header boundary**: Scan for `\r\n\r\n` (the blank line between headers
   and body)
2. **Parse status line**: `"HTTP/1.1 200 OK"` -> `(200, "OK")`
3. **Parse headers**: Each `"Name: value"` line -> `Vec<(String, String)>`
4. **Determine body length**: From `Content-Length` header, or take everything
   after headers if absent
5. **Return `HttpResponse`**: status, reason, headers, body

Two-phase parsing is also supported for streaming:
- `parse_headers(data)` returns headers and body start offset
- The caller reads body bytes incrementally

### Chunked Transfer Encoding Decoder

`decode_chunked()` handles HTTP chunked transfer encoding (used by streaming
API responses):

```
Wire format:           Decoded:
5\r\n                  hello world
hello\r\n
6\r\n
 world\r\n
0\r\n
\r\n
```

Each chunk: hex size, `\r\n`, data, `\r\n`. Size 0 terminates.

### Convenience Helpers

Two helper functions build pre-configured requests:

```rust
// Anthropic Messages API request
anthropic_messages_request(api_key, body_json) -> HttpRequest
// Headers: Content-Type, x-api-key, anthropic-version, Accept: text/event-stream

// OAuth device code request
oauth_device_code_request(client_id) -> HttpRequest
// POST to auth.anthropic.com/oauth/device/code
```

---

## SSE Parser

**Source:** `crates/net/src/http.rs` (bottom section)

Server-Sent Events (SSE) are the streaming format used by the Anthropic Messages
API when `"stream": true` is set.

### SSE Wire Format

```
event: content_block_delta\n
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}\n
\n
event: message_stop\n
data: {"type":"message_stop"}\n
\n
```

Events are delimited by blank lines. Each event has `field: value` lines where
field is `event`, `data`, `id`, or `retry`. ClaudioOS only uses `event` and `data`.

### Parser API

```rust
pub fn parse_sse_events(data: &[u8]) -> (Vec<SseEvent>, usize)
```

- Input: raw bytes from the HTTP response body
- Output: vector of complete events + number of bytes consumed
- Incomplete events at the end of the buffer are NOT consumed (left for next call)

### SseEvent

```rust
pub struct SseEvent {
    pub event: String,   // e.g., "content_block_delta", "message_stop"
    pub data: String,    // JSON payload (multiple data: lines joined with \n)
}
```

### Incremental Buffer Strategy

SSE events can arrive split across TCP segments. The consumer maintains a buffer
and calls `parse_sse_events()` repeatedly:

```rust
let mut buffer = Vec::new();
loop {
    let n = tls.recv(&mut stack, &mut tmp, now)?;
    buffer.extend_from_slice(&tmp[..n]);

    let (events, consumed) = parse_sse_events(&buffer);
    buffer.drain(..consumed);

    for event in events {
        handle_event(event);
    }
}
```

### Test Coverage

The HTTP module includes host-side unit tests (`cargo test` in the `crates/net/`
directory):

- Request serialization format
- Response parsing (complete + incomplete)
- Chunked encoding decoding
- SSE event parsing
- Case-insensitive header lookup
