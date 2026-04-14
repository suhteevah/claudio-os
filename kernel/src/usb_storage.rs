//! USB Mass Storage adapter — bridges xHCI bulk endpoints to the usb-storage
//! crate's `BulkTransport` trait.
//!
//! `XhciBulkTransport` holds the slot ID and mass storage endpoint info for a
//! single USB mass storage device. Each `BulkTransport` method locks the global
//! `XHCI` mutex and performs the transfer through the xHCI controller.

extern crate alloc;

use claudio_xhci::MassStorageInfo;
use claudio_usb_storage::bot::{BotError, BulkTransport};

/// Adapter bridging xHCI bulk endpoints to the usb-storage `BulkTransport` trait.
///
/// Holds per-device state (slot ID, endpoint DCIs) but accesses the shared
/// `XhciController` through the global `crate::usb::XHCI` mutex on every call.
pub struct XhciBulkTransport {
    slot_id: u8,
    info: MassStorageInfo,
}

impl XhciBulkTransport {
    /// Create a new transport adapter for the given xHCI slot and mass storage info.
    pub fn new(slot_id: u8, info: MassStorageInfo) -> Self {
        log::info!(
            "usb-storage: XhciBulkTransport created for slot={} bulk_in_dci={} bulk_out_dci={}",
            slot_id, info.bulk_in_dci, info.bulk_out_dci,
        );
        Self { slot_id, info }
    }
}

impl BulkTransport for XhciBulkTransport {
    fn bulk_out(&self, data: &[u8]) -> Result<usize, BotError> {
        let mut guard = crate::usb::XHCI.lock();
        let ctrl = guard.as_mut().ok_or(BotError::CbwTransferFailed)?;
        ctrl.0
            .bulk_out(self.slot_id, self.info.bulk_out_dci, data)
            .map_err(|e| {
                log::error!("usb-storage: bulk_out failed: {:?}", e);
                BotError::DataTransferFailed
            })
    }

    fn bulk_in(&self, buf: &mut [u8]) -> Result<usize, BotError> {
        let mut guard = crate::usb::XHCI.lock();
        let ctrl = guard.as_mut().ok_or(BotError::CswTransferFailed)?;
        ctrl.0
            .bulk_in(self.slot_id, self.info.bulk_in_dci, buf)
            .map_err(|e| {
                log::error!("usb-storage: bulk_in failed: {:?}", e);
                BotError::DataTransferFailed
            })
    }

    fn mass_storage_reset(&self) -> Result<(), BotError> {
        let mut guard = crate::usb::XHCI.lock();
        let ctrl = guard.as_mut().ok_or(BotError::ResetFailed)?;
        ctrl.0
            .mass_storage_reset(self.slot_id, self.info.interface_num)
            .map_err(|e| {
                log::error!("usb-storage: mass_storage_reset failed: {:?}", e);
                BotError::ResetFailed
            })
    }

    fn clear_halt(&self, endpoint: u8) -> Result<(), BotError> {
        let mut guard = crate::usb::XHCI.lock();
        let ctrl = guard.as_mut().ok_or(BotError::ClearHaltFailed)?;
        // The endpoint parameter is a USB endpoint address (with direction bit).
        // Map it to the appropriate xHCI endpoint address.
        let ep_addr = if endpoint & 0x80 != 0 {
            // IN endpoint — reconstruct the USB endpoint address from the bulk_in DCI
            // DCI for IN = ep_num * 2 + 1, so ep_num = (dci - 1) / 2
            let ep_num = (self.info.bulk_in_dci - 1) / 2;
            0x80 | ep_num
        } else {
            // OUT endpoint — DCI for OUT = ep_num * 2, so ep_num = dci / 2
            let ep_num = self.info.bulk_out_dci / 2;
            ep_num
        };

        ctrl.0
            .clear_endpoint_halt(self.slot_id, ep_addr)
            .map_err(|e| {
                log::error!("usb-storage: clear_halt endpoint={:#x} failed: {:?}", ep_addr, e);
                BotError::ClearHaltFailed
            })
    }

    fn bulk_in_endpoint(&self) -> u8 {
        // Reconstruct USB endpoint address from DCI
        // bulk_in DCI = ep_num * 2 + 1, so ep_num = (dci - 1) / 2
        let ep_num = (self.info.bulk_in_dci - 1) / 2;
        0x80 | ep_num
    }

    fn bulk_out_endpoint(&self) -> u8 {
        // bulk_out DCI = ep_num * 2, so ep_num = dci / 2
        self.info.bulk_out_dci / 2
    }
}
