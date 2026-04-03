//! LZNT1 decompression for NTFS compressed attributes.
//!
//! NTFS uses LZNT1 compression for attributes marked with ATTR_FLAG_COMPRESSED
//! (0x0001). Data is divided into 4KB compression units. Each unit has a 2-byte
//! header:
//! - Bit 15: compressed flag (1 = compressed, 0 = uncompressed)
//! - Bits 0-11: chunk data size minus 1 (so actual size = field + 1)
//!
//! Compressed chunks contain a sequence of flag groups. Each group starts with
//! a 1-byte flag, where each bit indicates whether the corresponding token is
//! a literal byte (0) or a back-reference (1). Back-references encode an
//! (offset, length) pair with variable-width fields based on the current
//! decompressed position within the chunk.
//!
//! Reference: MS-XCA section 2.5 (LZNT1 algorithm)

use alloc::vec::Vec;

/// Size of one LZNT1 compression unit (uncompressed).
pub const COMPRESSION_UNIT_SIZE: usize = 4096;

/// Decompress LZNT1-compressed data.
///
/// `compressed` is the raw compressed data from disk.
/// `uncompressed_size` is the expected output size (from the attribute's data_size).
///
/// Returns the decompressed data or `None` on error.
pub fn lznt1_decompress(compressed: &[u8], uncompressed_size: usize) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(uncompressed_size);
    let mut pos = 0;

    log::debug!("[ntfs::compression] LZNT1 decompress: {} compressed bytes -> {} expected",
        compressed.len(), uncompressed_size);

    while pos < compressed.len() && output.len() < uncompressed_size {
        // Need at least 2 bytes for the chunk header
        if pos + 2 > compressed.len() {
            break;
        }

        let header = u16::from_le_bytes([compressed[pos], compressed[pos + 1]]);
        pos += 2;

        if header == 0 {
            // End of compressed data
            log::trace!("[ntfs::compression] zero header at pos {}, end of data", pos - 2);
            break;
        }

        let chunk_data_size = (header & 0x0FFF) as usize + 1;
        let is_compressed = header & 0x8000 != 0;

        // Signature bits (bits 12-14) should be 0b011 for 4KB chunks
        // but we don't strictly validate this

        if pos + chunk_data_size > compressed.len() {
            log::error!("[ntfs::compression] chunk extends beyond buffer: pos={}, chunk_size={}, buf_len={}",
                pos, chunk_data_size, compressed.len());
            return None;
        }

        if !is_compressed {
            // Uncompressed chunk: raw copy
            let to_copy = chunk_data_size.min(uncompressed_size - output.len());
            output.extend_from_slice(&compressed[pos..pos + to_copy]);
            log::trace!("[ntfs::compression] uncompressed chunk: {} bytes", to_copy);
            pos += chunk_data_size;
        } else {
            // Compressed chunk: decompress
            let chunk_end = pos + chunk_data_size;
            let chunk_start_output = output.len();

            if !decompress_chunk(compressed, pos, chunk_end, &mut output, chunk_start_output) {
                log::error!("[ntfs::compression] failed to decompress chunk at offset {}", pos);
                return None;
            }

            log::trace!("[ntfs::compression] compressed chunk: {} -> {} bytes",
                chunk_data_size, output.len() - chunk_start_output);
            pos = chunk_end;
        }
    }

    // Pad to expected size with zeros if needed (can happen with sparse trailing data)
    if output.len() < uncompressed_size {
        output.resize(uncompressed_size, 0);
    }

    // Truncate if we got more than expected
    output.truncate(uncompressed_size);

    log::debug!("[ntfs::compression] decompressed {} bytes", output.len());
    Some(output)
}

/// Decompress a single LZNT1 chunk.
///
/// Returns true on success.
fn decompress_chunk(
    compressed: &[u8],
    mut pos: usize,
    chunk_end: usize,
    output: &mut Vec<u8>,
    chunk_start_output: usize,
) -> bool {
    while pos < chunk_end {
        // Read the flag byte
        if pos >= chunk_end {
            break;
        }
        let flags = compressed[pos];
        pos += 1;

        // Process 8 tokens (one per flag bit)
        for bit in 0..8 {
            if pos >= chunk_end {
                break;
            }

            if flags & (1 << bit) == 0 {
                // Literal byte
                output.push(compressed[pos]);
                pos += 1;
            } else {
                // Back-reference: (offset, length) pair
                if pos + 2 > chunk_end {
                    log::error!("[ntfs::compression] truncated back-reference at pos {}", pos);
                    return false;
                }

                let token = u16::from_le_bytes([compressed[pos], compressed[pos + 1]]);
                pos += 2;

                // The number of offset bits depends on the current position
                // within the uncompressed chunk
                let decompressed_pos = output.len() - chunk_start_output;
                let offset_bits = compute_offset_bits(decompressed_pos);
                let length_bits = 16 - offset_bits;

                let length_mask = (1u16 << length_bits) - 1;
                let length = (token & length_mask) as usize + 3; // minimum length is 3
                let offset = ((token >> length_bits) as usize) + 1; // minimum offset is 1

                if offset > decompressed_pos {
                    log::error!("[ntfs::compression] back-reference offset {} exceeds decompressed size {}",
                        offset, decompressed_pos);
                    return false;
                }

                // Copy bytes from the back-reference
                let src_start = output.len() - offset;
                for i in 0..length {
                    let byte = output[src_start + (i % offset)];
                    output.push(byte);
                }
            }
        }
    }

    true
}

/// Compute the number of bits used for the offset field in a back-reference,
/// based on the current decompressed position within the chunk.
///
/// LZNT1 uses a variable-width encoding: more offset bits when further into
/// the chunk (since back-references can reach further back).
///
/// Position range -> offset bits:
///   0..=0       -> 4  (but no back-refs at pos 0)
///   1..=1       -> 4  (offset 1, max)
///   ...the formula is: offset_bits = max(4, ceil(log2(pos)))
fn compute_offset_bits(pos: usize) -> u16 {
    if pos < 0x10 {
        4
    } else if pos < 0x20 {
        5
    } else if pos < 0x40 {
        6
    } else if pos < 0x80 {
        7
    } else if pos < 0x100 {
        8
    } else if pos < 0x200 {
        9
    } else if pos < 0x400 {
        10
    } else if pos < 0x800 {
        11
    } else {
        12
    }
}

/// Check if an attribute's data needs LZNT1 decompression.
///
/// An attribute needs decompression if:
/// 1. The ATTR_FLAG_COMPRESSED (0x0001) flag is set
/// 2. The compression_unit in the non-resident header is > 0
#[inline]
pub fn needs_decompression(attr_flags: u16, compression_unit: u16) -> bool {
    (attr_flags & crate::attribute::ATTR_FLAG_COMPRESSED != 0) && compression_unit > 0
}

/// Decompress a full non-resident attribute's data.
///
/// `raw_data` is the data read from the clusters (may contain compressed chunks).
/// `data_size` is the expected uncompressed size from the non-resident header.
/// `compression_unit` is the log2 of the compression unit size in clusters.
/// `cluster_size` is the volume's cluster size.
///
/// For NTFS, the compression unit is typically 2^4 = 16 clusters = 64KB when
/// cluster_size = 4096. However, the individual LZNT1 chunks within are 4KB each.
pub fn decompress_attribute(
    raw_data: &[u8],
    data_size: u64,
    _compression_unit: u16,
    _cluster_size: u64,
) -> Option<Vec<u8>> {
    lznt1_decompress(raw_data, data_size as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uncompressed_chunk() {
        // Build an uncompressed chunk: header with bit 15 clear
        let data = [0x41u8; 4096]; // 4096 'A's
        let mut compressed = Vec::new();
        // Chunk header: size = 4095 (0x0FFF), not compressed (bit 15 = 0)
        // Signature bits 12-14 = 0b011 -> 0x3FFF
        let header: u16 = 0x3FFF; // 0011_1111_1111_1111 = not compressed, size=4096
        compressed.extend_from_slice(&header.to_le_bytes());
        compressed.extend_from_slice(&data);

        let result = lznt1_decompress(&compressed, 4096).unwrap();
        assert_eq!(result.len(), 4096);
        assert!(result.iter().all(|&b| b == 0x41));
    }

    #[test]
    fn test_compressed_literals_only() {
        // Build a compressed chunk with only literal bytes (no back-references)
        let mut chunk_data = Vec::new();
        // We'll write 8 literal bytes: flag byte 0x00 (all literals), then 8 bytes
        chunk_data.push(0x00); // flag: all 8 tokens are literals
        chunk_data.extend_from_slice(&[0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21, 0x21, 0x21]); // "Hello!!!"

        let mut compressed = Vec::new();
        // Chunk header: compressed, size = chunk_data.len() - 1
        let header: u16 = 0x8000 | ((chunk_data.len() as u16) - 1);
        // Add signature bits (bits 12-14 = 0b011) -> set bits 13,12
        let header = header | 0x3000;
        compressed.extend_from_slice(&header.to_le_bytes());
        compressed.extend_from_slice(&chunk_data);

        let result = lznt1_decompress(&compressed, 8).unwrap();
        assert_eq!(result, b"Hello!!!");
    }

    #[test]
    fn test_offset_bits() {
        assert_eq!(compute_offset_bits(0), 4);
        assert_eq!(compute_offset_bits(1), 4);
        assert_eq!(compute_offset_bits(15), 4);
        assert_eq!(compute_offset_bits(16), 5);
        assert_eq!(compute_offset_bits(31), 5);
        assert_eq!(compute_offset_bits(32), 6);
        assert_eq!(compute_offset_bits(0x800), 12);
    }

    #[test]
    fn test_zero_header_terminates() {
        let compressed = [0u8; 4];
        let result = lznt1_decompress(&compressed, 0).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_decompress_with_backreference() {
        // Build a compressed chunk: "AAAA" using a literal 'A' then back-ref
        let mut chunk_data = Vec::new();

        // Flag byte: bit 0 = literal, bit 1 = back-reference
        chunk_data.push(0x02); // 0b00000010

        // Literal: 'A' (0x41)
        chunk_data.push(0x41);

        // Back-reference: at decompressed pos=1, offset_bits=4, length_bits=12
        // offset=1 (encoded as 0), length=3 (encoded as 0, since min length=3)
        // token = (offset_encoded << length_bits) | length_encoded
        // offset_encoded = offset - 1 = 0
        // length_encoded = length - 3 = 0
        // token = (0 << 12) | 0 = 0x0000
        chunk_data.push(0x00);
        chunk_data.push(0x00);

        // Remaining bits in the flag are 0 (literals) but we don't have more data
        // so the loop will stop at chunk_end.

        let mut compressed = Vec::new();
        let header: u16 = 0xB000 | ((chunk_data.len() as u16) - 1);
        compressed.extend_from_slice(&header.to_le_bytes());
        compressed.extend_from_slice(&chunk_data);

        let result = lznt1_decompress(&compressed, 4).unwrap();
        assert_eq!(result, b"AAAA");
    }
}
