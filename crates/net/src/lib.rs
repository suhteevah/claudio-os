//! ClaudioOS network stack: VirtIO-net NIC driver, smoltcp TCP/IP, DNS, TLS,
//! and raw HTTP/1.1 client.
//!
//! # Architecture
//!
//! ```text
//!   HTTP client (http.rs)
//!       ↓ raw bytes
//!   TLS stream (tls.rs)
//!       ↓ encrypted records
//!   TCP socket (smoltcp)
//!       ↓ segments
//!   IP layer (smoltcp)
//!       ↓ packets
//!   Ethernet (smoltcp)
//!       ↓ frames
//!   NIC driver (nic.rs) ←→ VirtIO virtqueues ←→ hardware/QEMU
//! ```
//!
//! # Usage from the kernel
//!
//! ```ignore
//! // After PCI enumeration finds VirtIO-net at some I/O base:
//! let nic = unsafe { VirtioNet::new(io_base, phys_mem_offset)? };
//! let mut stack = NetworkStack::new(nic);
//!
//! // Poll in a loop (driven by timer/NIC interrupts) until DHCP completes:
//! while !stack.has_ip {
//!     stack.poll(now());
//! }
//!
//! // Resolve a hostname:
//! let ip = dns::resolve(&mut stack, "api.anthropic.com", || now())?;
//!
//! // TCP connect + TLS handshake:
//! let tcp = tls::tcp_connect(&mut stack, ip, 443, 49152, || now())?;
//! let mut tls = TlsStream::handshake(&mut stack, tcp, "api.anthropic.com".into(), || now())?;
//!
//! // Send an HTTP request:
//! let req = http::HttpRequest::post("api.anthropic.com", "/v1/messages", body);
//! tls.send(&mut stack, &req.to_bytes(), || now())?;
//! ```

#![no_std]
extern crate alloc;

pub mod dns;
pub mod http;
pub mod nic;
pub mod stack;
pub mod tls;

// ---------------------------------------------------------------------------
// Core NIC trait — implemented by VirtIO-net and (future) e1000
// ---------------------------------------------------------------------------

/// Trait for network interface card drivers.
///
/// All NIC drivers in ClaudioOS implement this trait so the smoltcp device
/// adapter can be generic (though currently only VirtIO-net exists).
pub trait NicDriver {
    /// Transmit an Ethernet frame.
    ///
    /// `frame` contains a complete Ethernet frame (dest MAC + src MAC +
    /// ethertype + payload).  The driver prepends any device-specific headers
    /// (e.g. VirtIO-net header) internally.
    fn transmit(&mut self, frame: &[u8]) -> Result<(), NicError>;

    /// Receive an Ethernet frame into `buf`.
    ///
    /// Returns `Ok(Some(len))` if a frame was received, `Ok(None)` if no
    /// frame is available, or `Err` on device error.  The driver strips any
    /// device-specific headers and writes only the Ethernet frame.
    fn receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NicError>;

    /// The NIC's 6-byte MAC address.
    fn mac_address(&self) -> [u8; 6];
}

/// Errors from NIC operations.
#[derive(Debug)]
pub enum NicError {
    /// The transmit queue or receive buffer is full.
    BufferFull,
    /// The link is down (cable unplugged, etc.).
    LinkDown,
    /// A generic device error occurred.
    DeviceError,
}

/// Errors from high-level network initialization.
#[derive(Debug)]
pub enum InitError {
    /// VirtIO device initialization failed.
    NicInit(nic::VirtioInitError),
    /// DHCP lease acquisition timed out.
    DhcpTimeout,
}

// ---------------------------------------------------------------------------
// Re-exports for convenience
// ---------------------------------------------------------------------------

pub use dns::DnsError;
pub use http::{HttpError, HttpRequest, HttpResponse, SseEvent};
pub use nic::VirtioNet;
pub use stack::NetworkStack;
pub use tls::{TcpError, TlsError, TlsStream};

/// Re-export smoltcp's Instant type so the kernel can construct timestamps
/// without depending on smoltcp directly.
pub use smoltcp::time::Instant;

// ---------------------------------------------------------------------------
// PCI device info (mirror of kernel's PciDevice, kept minimal to avoid
// cross-crate dependency on kernel internals)
// ---------------------------------------------------------------------------

/// Minimal PCI device descriptor passed from the kernel's PCI scanner.
#[derive(Debug, Clone, Copy)]
pub struct PciDeviceInfo {
    /// I/O port base address (BAR0 with indicator bits stripped).
    pub io_base: u16,
    /// PCI interrupt line (IRQ number).
    pub irq_line: u8,
}

// ---------------------------------------------------------------------------
// High-level initialization
// ---------------------------------------------------------------------------

/// Maximum number of poll iterations while waiting for DHCP.
const DHCP_TIMEOUT_POLLS: usize = 100_000;

/// Initialize the network stack end-to-end.
///
/// 1. Creates the VirtIO-net driver using PCI device info.
/// 2. Wraps it in a smoltcp `Interface` with DHCP.
/// 3. Polls until a DHCP lease is acquired.
///
/// # Arguments
/// * `pci_dev` -- PCI device info (I/O base from BAR0, IRQ line).
/// * `phys_mem_offset` -- the bootloader's physical memory offset for
///   virt->phys address translation.
/// * `now` -- a function returning the current [`smoltcp::time::Instant`].
///
/// # Safety
/// The caller must ensure the PCI device info points to a valid VirtIO-net
/// device and that `phys_mem_offset` is correct. PCI bus mastering must be
/// enabled before calling this.
pub unsafe fn init(
    pci_dev: PciDeviceInfo,
    phys_mem_offset: u64,
    now: impl Fn() -> smoltcp::time::Instant,
) -> Result<NetworkStack, InitError> {
    log::info!(
        "[net] initializing with I/O base {:#x}, IRQ {}",
        pci_dev.io_base,
        pci_dev.irq_line
    );

    // Step 1: Initialize the NIC driver.
    let nic = unsafe {
        VirtioNet::new(pci_dev.io_base, phys_mem_offset).map_err(InitError::NicInit)?
    };

    // Step 2: Create the smoltcp network stack with DHCP.
    let mut stack = NetworkStack::new(nic);

    // Step 3: Poll until DHCP assigns an IP address.
    log::info!("[net] waiting for DHCP lease...");
    for i in 0..DHCP_TIMEOUT_POLLS {
        stack.poll(now());

        if stack.has_ip {
            if let Some(addr) = stack.ipv4_addr() {
                log::info!("[net] network ready: IP {}", addr);
            }
            return Ok(stack);
        }

        if i > 0 && i % 10_000 == 0 {
            log::debug!("[net] still waiting for DHCP ({} polls)...", i);
        }
    }

    log::error!("[net] DHCP timed out after {} polls", DHCP_TIMEOUT_POLLS);
    Err(InitError::DhcpTimeout)

}

/// Initialize the network stack using raw I/O base and phys_mem_offset.
///
/// Convenience wrapper for callers that have already extracted the I/O base
/// from PCI BAR0.
///
/// # Safety
/// Same as [`init`].
pub unsafe fn init_from_io_base(
    io_base: u16,
    phys_mem_offset: u64,
    now: impl Fn() -> smoltcp::time::Instant,
) -> Result<NetworkStack, InitError> {
    unsafe {
        init(
            PciDeviceInfo {
                io_base,
                irq_line: 0,
            },
            phys_mem_offset,
            now,
        )
    }
}
