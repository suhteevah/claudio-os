//! Firmware loading for Intel WiFi devices.
//!
//! # Firmware requirement
//!
//! Intel WiFi hardware is completely inert without firmware microcode. The chip
//! contains a minimal bootstrap ROM that can load firmware into SRAM via the
//! BSM (Bootstrap State Machine), but all 802.11 MAC, PHY, and radio
//! functionality lives in the firmware image.
//!
//! ## Where to get firmware
//!
//! Firmware images ship with Linux (`/lib/firmware/`) and are also available from
//! <https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git>.
//!
//! For ClaudioOS, the firmware `.ucode` file must be placed on the FAT32 persist
//! partition under `/firmware/`. The expected filename depends on the device
//! variant — see [`WifiVariant::firmware_name()`](crate::WifiVariant::firmware_name).
//!
//! ## Firmware file format
//!
//! iwlwifi `.ucode` files use a TLV (Type-Length-Value) container format:
//!
//! ```text
//! Offset  Size  Description
//! ------  ----  -----------
//! 0x00    4     Magic: 0x0000 or IWL_TLV_UCODE_MAGIC (0x0a4c5749)
//! 0x04    4     Zero (padding)
//! 0x08    4     Alternative count
//! 0x0C    4     Version (API)
//! 0x10    4     Build number
//! 0x14    ...   TLV entries: [type:u32] [length:u32] [data:u8*length] [padding to 4]
//! ```
//!
//! Key TLV types:
//! - `IWL_UCODE_TLV_INST` (1): Instruction SRAM image
//! - `IWL_UCODE_TLV_DATA` (2): Data SRAM image
//! - `IWL_UCODE_TLV_INIT` (3): Init instruction image (runs once at boot)
//! - `IWL_UCODE_TLV_INIT_DATA` (4): Init data image
//! - `IWL_UCODE_TLV_FW_VERSION` (36): Firmware version string
//! - `IWL_UCODE_TLV_SEC` (50+): Paged firmware sections
//!
//! # Safety
//!
//! Firmware loading writes directly to device MMIO registers. The caller must
//! ensure exclusive access to the device and valid physical memory mappings.

use alloc::vec::Vec;

/// Parsed firmware image ready for upload to device SRAM.
#[derive(Debug)]
pub struct FirmwareImage {
    /// Instruction SRAM section.
    pub inst_section: Vec<u8>,
    /// Data SRAM section.
    pub data_section: Vec<u8>,
    /// Init instruction section (runs once during first boot).
    pub init_inst_section: Vec<u8>,
    /// Init data section.
    pub init_data_section: Vec<u8>,
    /// Firmware API version.
    pub api_version: u32,
    /// Build number.
    pub build: u32,
}

/// TLV type constants from iwlwifi.
#[repr(u32)]
#[allow(dead_code)]
enum TlvType {
    Inst = 1,
    Data = 2,
    Init = 3,
    InitData = 4,
    BootArg = 5,
    FwVersion = 36,
    SecRt = 50,
}

/// Magic value at the start of `.ucode` files.
const IWL_UCODE_TLV_MAGIC: u32 = 0x0A4C_5749; // "IWL\n" in little-endian

/// Parse a `.ucode` firmware image from raw bytes.
///
/// Returns `None` if the image is corrupt, too short, or has an unrecognized
/// magic value.
pub fn parse_firmware(data: &[u8]) -> Option<FirmwareImage> {
    if data.len() < 0x18 {
        log::error!("wifi::firmware: image too short ({} bytes)", data.len());
        return None;
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != IWL_UCODE_TLV_MAGIC && magic != 0 {
        log::error!("wifi::firmware: bad magic 0x{:08X}", magic);
        return None;
    }

    let api_version = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);
    let build = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]);

    log::info!(
        "wifi::firmware: parsing image — API version {} build {}",
        api_version, build
    );

    let mut fw = FirmwareImage {
        inst_section: Vec::new(),
        data_section: Vec::new(),
        init_inst_section: Vec::new(),
        init_data_section: Vec::new(),
        api_version,
        build,
    };

    // Walk TLV entries starting at offset 0x18
    let mut offset = 0x18usize;
    while offset + 8 <= data.len() {
        let tlv_type = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
        ]);
        let tlv_len = u32::from_le_bytes([
            data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7],
        ]) as usize;
        offset += 8;

        if offset + tlv_len > data.len() {
            log::warn!(
                "wifi::firmware: TLV type {} length {} extends past EOF at offset 0x{:X}",
                tlv_type, tlv_len, offset
            );
            break;
        }

        let payload = &data[offset..offset + tlv_len];

        match tlv_type {
            1 => {
                log::debug!("wifi::firmware: INST section — {} bytes", tlv_len);
                fw.inst_section = payload.to_vec();
            }
            2 => {
                log::debug!("wifi::firmware: DATA section — {} bytes", tlv_len);
                fw.data_section = payload.to_vec();
            }
            3 => {
                log::debug!("wifi::firmware: INIT_INST section — {} bytes", tlv_len);
                fw.init_inst_section = payload.to_vec();
            }
            4 => {
                log::debug!("wifi::firmware: INIT_DATA section — {} bytes", tlv_len);
                fw.init_data_section = payload.to_vec();
            }
            36 => {
                if let Ok(version_str) = core::str::from_utf8(payload) {
                    log::info!("wifi::firmware: version string = \"{}\"", version_str);
                }
            }
            _ => {
                log::trace!("wifi::firmware: skipping TLV type {} ({} bytes)", tlv_type, tlv_len);
            }
        }

        // Advance past payload, aligned to 4 bytes
        offset += (tlv_len + 3) & !3;
    }

    if fw.inst_section.is_empty() {
        log::error!("wifi::firmware: no instruction section found in image");
        return None;
    }

    log::info!(
        "wifi::firmware: parsed OK — inst={}B data={}B init_inst={}B init_data={}B",
        fw.inst_section.len(),
        fw.data_section.len(),
        fw.init_inst_section.len(),
        fw.init_data_section.len(),
    );

    Some(fw)
}

// -------------------------------------------------------------------
// BSM (Bootstrap State Machine) register offsets
// -------------------------------------------------------------------

/// BSM write pointer register.
const BSM_WR_CTRL: u32 = 0x3400;
/// BSM write pointer data register.
const BSM_WR_MEM_SRC: u32 = 0x3404;
/// BSM write pointer destination register.
const BSM_WR_MEM_DST: u32 = 0x3408;
/// BSM write pointer byte count.
const BSM_WR_DWCOUNT: u32 = 0x340C;
/// BSM status register.
const BSM_WR_STATUS: u32 = 0x3410;

/// BSM start command bit.
const BSM_WR_CTRL_START: u32 = 1 << 31;

/// Upload firmware sections to device SRAM via the BSM.
///
/// # Safety
///
/// - `mmio_base` must be a valid mapped pointer to the device's BAR0 MMIO region.
/// - Caller must have exclusive access to the device.
/// - The device must be in a state that accepts BSM writes (after NIC reset, before alive).
pub unsafe fn upload_to_device(mmio_base: *mut u8, fw: &FirmwareImage) -> Result<(), &'static str> {
    log::info!("wifi::firmware: uploading instruction section ({} bytes) via BSM", fw.inst_section.len());

    // Write the instruction image to BSM SRAM port
    let sram_base = mmio_base.add(0x1000) as *mut u32;
    for (i, chunk) in fw.inst_section.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let val = u32::from_le_bytes(word);
        core::ptr::write_volatile(sram_base.add(i), val);
    }

    // Configure BSM to copy from staging area to instruction SRAM
    let write_reg = |offset: u32, value: u32| {
        let ptr = mmio_base.add(offset as usize) as *mut u32;
        core::ptr::write_volatile(ptr, value);
    };

    let read_reg = |offset: u32| -> u32 {
        let ptr = mmio_base.add(offset as usize) as *const u32;
        core::ptr::read_volatile(ptr)
    };

    write_reg(BSM_WR_MEM_SRC, 0);
    write_reg(BSM_WR_MEM_DST, 0);
    write_reg(BSM_WR_DWCOUNT, (fw.inst_section.len() / 4) as u32);
    write_reg(BSM_WR_CTRL, BSM_WR_CTRL_START);

    // Poll BSM for completion
    for _ in 0..1000 {
        let status = read_reg(BSM_WR_STATUS);
        if status & 1 != 0 {
            log::info!("wifi::firmware: BSM upload complete");
            return Ok(());
        }
        // Busy-wait (in practice we'd yield to the async executor)
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    log::error!("wifi::firmware: BSM upload timed out");
    Err("firmware BSM upload timed out")
}
