//! NTFS data run (mapping pairs) decoding and encoding.
//!
//! Non-resident attributes store their cluster locations using a compact
//! variable-length encoding called "mapping pairs" or "data runs". Each run
//! encodes a (length, offset) pair where the length is the number of contiguous
//! clusters and the offset is the signed delta from the previous run's LCN.
//!
//! Encoding format:
//! - First byte: `(offset_size << 4) | length_size`
//!   - `length_size`: number of bytes for the run length (1-4)
//!   - `offset_size`: number of bytes for the run offset (0-4, 0 = sparse)
//! - Next `length_size` bytes: run length (unsigned, little-endian)
//! - Next `offset_size` bytes: run offset (signed, little-endian, relative to previous LCN)
//! - Terminated by a zero byte
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/concepts/data_runs.html>

use alloc::vec::Vec;

/// A single decoded data run (cluster run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRun {
    /// Starting Logical Cluster Number (absolute).
    /// For sparse runs, this is 0 and `is_sparse` is true.
    pub lcn: u64,
    /// Number of clusters in this run.
    pub length: u64,
    /// Whether this is a sparse (unallocated) run.
    pub is_sparse: bool,
}

impl DataRun {
    /// Byte offset of this run on the volume.
    #[inline]
    pub fn byte_offset(&self, cluster_size: u64) -> u64 {
        self.lcn * cluster_size
    }

    /// Total byte length of this run.
    #[inline]
    pub fn byte_length(&self, cluster_size: u64) -> u64 {
        self.length * cluster_size
    }
}

/// A complete VCN-to-LCN mapping built from all data runs.
#[derive(Debug, Clone)]
pub struct VcnToLcnMap {
    /// Ordered list of data runs with their starting VCNs.
    pub entries: Vec<VcnMapEntry>,
}

/// A single entry in the VCN-to-LCN mapping.
#[derive(Debug, Clone, Copy)]
pub struct VcnMapEntry {
    /// Starting Virtual Cluster Number for this run.
    pub vcn: u64,
    /// Starting Logical Cluster Number (0 if sparse).
    pub lcn: u64,
    /// Number of clusters in this run.
    pub length: u64,
    /// Whether this run is sparse.
    pub is_sparse: bool,
}

/// Decode data runs (mapping pairs) from raw bytes.
///
/// Returns a list of `DataRun` with absolute LCN values.
/// The encoding terminates when a zero byte is encountered.
pub fn decode_data_runs(buf: &[u8]) -> Vec<DataRun> {
    let mut runs = Vec::new();
    let mut pos = 0;
    let mut prev_lcn: i64 = 0;

    log::trace!("[ntfs::data_runs] decoding {} bytes of mapping pairs", buf.len());

    loop {
        if pos >= buf.len() {
            log::trace!("[ntfs::data_runs] reached end of buffer at position {}", pos);
            break;
        }

        let header = buf[pos];
        if header == 0 {
            log::trace!("[ntfs::data_runs] terminator at position {}", pos);
            break;
        }
        pos += 1;

        let length_size = (header & 0x0F) as usize;
        let offset_size = ((header >> 4) & 0x0F) as usize;

        if length_size == 0 || length_size > 8 {
            log::error!("[ntfs::data_runs] invalid length_size: {} at position {}", length_size, pos - 1);
            break;
        }
        if offset_size > 8 {
            log::error!("[ntfs::data_runs] invalid offset_size: {} at position {}", offset_size, pos - 1);
            break;
        }

        if pos + length_size + offset_size > buf.len() {
            log::error!("[ntfs::data_runs] run extends beyond buffer: need {} bytes at position {}, have {}",
                length_size + offset_size, pos, buf.len() - pos);
            break;
        }

        // Read run length (unsigned)
        let run_length = read_unsigned(&buf[pos..pos + length_size]);
        pos += length_size;

        // Read run offset (signed, relative to previous LCN)
        let is_sparse;
        let lcn;

        if offset_size == 0 {
            // Sparse run: no offset field means this run is unallocated
            is_sparse = true;
            lcn = 0u64;
            log::trace!("[ntfs::data_runs] sparse run: length={} clusters", run_length);
        } else {
            is_sparse = false;
            let offset_delta = read_signed(&buf[pos..pos + offset_size]);
            pos += offset_size;

            prev_lcn += offset_delta;
            if prev_lcn < 0 {
                log::error!("[ntfs::data_runs] negative LCN after delta: {} + {} = {}",
                    prev_lcn - offset_delta, offset_delta, prev_lcn);
                break;
            }
            lcn = prev_lcn as u64;

            log::trace!("[ntfs::data_runs] run: lcn={}, length={} clusters (delta={})",
                lcn, run_length, offset_delta);
        }

        runs.push(DataRun {
            lcn,
            length: run_length,
            is_sparse,
        });
    }

    log::debug!("[ntfs::data_runs] decoded {} data runs", runs.len());
    runs
}

/// Build a complete VCN-to-LCN mapping from a list of data runs.
pub fn build_vcn_map(runs: &[DataRun]) -> VcnToLcnMap {
    let mut entries = Vec::with_capacity(runs.len());
    let mut vcn = 0u64;

    for run in runs {
        entries.push(VcnMapEntry {
            vcn,
            lcn: run.lcn,
            length: run.length,
            is_sparse: run.is_sparse,
        });
        vcn += run.length;
    }

    log::trace!("[ntfs::data_runs] built VCN map with {} entries spanning {} clusters",
        entries.len(), vcn);

    VcnToLcnMap { entries }
}

impl VcnToLcnMap {
    /// Resolve a VCN to its corresponding LCN.
    ///
    /// Returns `None` if the VCN is out of range.
    /// Returns `Some((lcn, is_sparse))` where lcn is 0 for sparse regions.
    pub fn resolve(&self, vcn: u64) -> Option<(u64, bool)> {
        for entry in &self.entries {
            if vcn >= entry.vcn && vcn < entry.vcn + entry.length {
                if entry.is_sparse {
                    return Some((0, true));
                }
                let offset = vcn - entry.vcn;
                return Some((entry.lcn + offset, false));
            }
        }
        log::warn!("[ntfs::data_runs] VCN {} not found in mapping", vcn);
        None
    }

    /// Total number of clusters covered by this mapping.
    pub fn total_clusters(&self) -> u64 {
        self.entries.iter().map(|e| e.length).sum()
    }
}

/// Encode a list of data runs into mapping pairs bytes.
///
/// This is the reverse of `decode_data_runs`, producing the on-disk format.
pub fn encode_data_runs(runs: &[DataRun]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut prev_lcn: i64 = 0;

    for run in runs {
        let length_bytes = min_unsigned_bytes(run.length);
        let (offset_bytes, offset_delta) = if run.is_sparse {
            (0usize, 0i64)
        } else {
            let delta = run.lcn as i64 - prev_lcn;
            prev_lcn = run.lcn as i64;
            (min_signed_bytes(delta), delta)
        };

        // Header byte
        let header = (offset_bytes as u8) << 4 | (length_bytes as u8);
        buf.push(header);

        // Length (unsigned, little-endian)
        write_unsigned(&mut buf, run.length, length_bytes);

        // Offset (signed, little-endian)
        if offset_bytes > 0 {
            write_signed(&mut buf, offset_delta, offset_bytes);
        }

        log::trace!("[ntfs::data_runs] encoded run: lcn={}, length={}, header=0x{:02X}",
            run.lcn, run.length, header);
    }

    // Terminator
    buf.push(0);
    log::debug!("[ntfs::data_runs] encoded {} runs into {} bytes", runs.len(), buf.len());
    buf
}

// --- Internal helpers ---

/// Read an unsigned integer of 1-8 bytes in little-endian.
fn read_unsigned(buf: &[u8]) -> u64 {
    let mut val = 0u64;
    for (i, &byte) in buf.iter().enumerate() {
        val |= (byte as u64) << (i * 8);
    }
    val
}

/// Read a signed integer of 1-8 bytes in little-endian (sign-extended).
fn read_signed(buf: &[u8]) -> i64 {
    let mut val = 0u64;
    for (i, &byte) in buf.iter().enumerate() {
        val |= (byte as u64) << (i * 8);
    }
    // Sign-extend: if the highest bit of the last byte is set, fill upper bits with 1s
    let bits = buf.len() * 8;
    if bits < 64 && (buf[buf.len() - 1] & 0x80) != 0 {
        val |= !0u64 << bits;
    }
    val as i64
}

/// Minimum number of bytes to represent an unsigned value.
fn min_unsigned_bytes(val: u64) -> usize {
    if val == 0 { return 1; }
    let bits = 64 - val.leading_zeros() as usize;
    (bits + 7) / 8
}

/// Minimum number of bytes to represent a signed value.
fn min_signed_bytes(val: i64) -> usize {
    if val == 0 { return 1; }
    if val > 0 {
        let bits = 64 - val.leading_zeros() as usize;
        // Need extra bit for sign
        (bits + 1 + 7) / 8
    } else {
        // For negative, count leading ones
        let bits = 64 - (!val).leading_zeros() as usize;
        (bits + 1 + 7) / 8
    }
}

/// Write an unsigned value as `count` little-endian bytes.
fn write_unsigned(buf: &mut Vec<u8>, val: u64, count: usize) {
    for i in 0..count {
        buf.push((val >> (i * 8)) as u8);
    }
}

/// Write a signed value as `count` little-endian bytes.
fn write_signed(buf: &mut Vec<u8>, val: i64, count: usize) {
    let uval = val as u64;
    for i in 0..count {
        buf.push((uval >> (i * 8)) as u8);
    }
}
