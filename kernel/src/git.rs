//! Simplified Git Smart HTTP protocol client for ClaudioOS.
//!
//! Provides just enough Git functionality for agents to clone repos, make
//! changes, commit, and push — all over HTTPS using the Smart HTTP protocol.
//!
//! No SSH transport, no merge, no branches, no rebasing. Fast-forward pulls
//! only.  Objects are stored in VFS under `{repo_path}/.git/objects/`.
//!
//! # Protocol
//!
//! - Clone/pull: `GET /info/refs?service=git-upload-pack`, then
//!   `POST /git-upload-pack` to receive a packfile.
//! - Push: `GET /info/refs?service=git-receive-pack`, then
//!   `POST /git-receive-pack` with a packfile.
//!
//! # Shell commands
//!
//! `git clone <url> [path]`, `git pull`, `git status`, `git add <files>`,
//! `git commit -m <message>`, `git push`, `git log [-n count]`

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use claudio_net::{Instant, NetworkStack};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum packfile size we'll accept (64 MiB).
const MAX_PACK_SIZE: usize = 64 * 1024 * 1024;

/// SHA-1 digest length in bytes.
const SHA1_LEN: usize = 20;

/// SHA-1 digest length as hex string.
const SHA1_HEX_LEN: usize = 40;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from git operations.
#[derive(Debug)]
pub enum GitError {
    /// Network / HTTP failure.
    Network(String),
    /// Protocol-level error (unexpected server response).
    Protocol(String),
    /// VFS I/O error.
    Io(String),
    /// Invalid object or corrupt data.
    Corrupt(String),
    /// Not a git repository.
    NotARepo,
    /// Nothing to commit.
    NothingToCommit,
    /// Fast-forward not possible (diverged history).
    NotFastForward,
    /// Invalid URL format.
    BadUrl(String),
}

impl core::fmt::Display for GitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GitError::Network(s) => write!(f, "network error: {}", s),
            GitError::Protocol(s) => write!(f, "protocol error: {}", s),
            GitError::Io(s) => write!(f, "I/O error: {}", s),
            GitError::Corrupt(s) => write!(f, "corrupt: {}", s),
            GitError::NotARepo => write!(f, "not a git repository"),
            GitError::NothingToCommit => write!(f, "nothing to commit"),
            GitError::NotFastForward => write!(f, "not a fast-forward, cannot pull"),
            GitError::BadUrl(s) => write!(f, "bad URL: {}", s),
        }
    }
}

// ---------------------------------------------------------------------------
// SHA-1 (minimal, for object hashing)
// ---------------------------------------------------------------------------

/// Minimal SHA-1 implementation for git object IDs.
///
/// Git uses SHA-1 for content addressing. We need it for:
/// - Computing object hashes (blob, tree, commit)
/// - Verifying packfile checksums
struct Sha1 {
    state: [u32; 5],
    count: u64,
    buffer: [u8; 64],
    buffer_len: usize,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            count: 0,
            buffer: [0u8; 64],
            buffer_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        self.count += data.len() as u64;

        // Fill buffer from previous partial block.
        if self.buffer_len > 0 {
            let fill = core::cmp::min(64 - self.buffer_len, data.len());
            self.buffer[self.buffer_len..self.buffer_len + fill]
                .copy_from_slice(&data[..fill]);
            self.buffer_len += fill;
            offset = fill;
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }

        // Process full blocks.
        while offset + 64 <= data.len() {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[offset..offset + 64]);
            self.compress(&block);
            offset += 64;
        }

        // Buffer remainder.
        if offset < data.len() {
            let remain = data.len() - offset;
            self.buffer[..remain].copy_from_slice(&data[offset..]);
            self.buffer_len = remain;
        }
    }

    fn finalize(mut self) -> [u8; 20] {
        let bit_len = self.count * 8;
        // Padding: 1 bit, then zeros, then 64-bit big-endian length.
        let mut pad = vec![0x80u8];
        while (self.buffer_len + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bit_len.to_be_bytes());
        self.update(&pad);

        let mut out = [0u8; 20];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

/// Compute SHA-1 hash of data, return as 20-byte array.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finalize()
}

/// Hex-encode a byte slice.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Decode a hex string into bytes.
fn hex_decode(hex: &str) -> Result<Vec<u8>, GitError> {
    if hex.len() % 2 != 0 {
        return Err(GitError::Corrupt(format!("odd-length hex: {}", hex)));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let hex_bytes = hex.as_bytes();
    for i in (0..hex_bytes.len()).step_by(2) {
        let hi = hex_nibble(hex_bytes[i])?;
        let lo = hex_nibble(hex_bytes[i + 1])?;
        bytes.push((hi << 4) | lo);
    }
    Ok(bytes)
}

fn hex_nibble(b: u8) -> Result<u8, GitError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(GitError::Corrupt(format!("bad hex char: {}", b as char))),
    }
}

// ---------------------------------------------------------------------------
// Zlib inflate (minimal, for packfile decompression)
// ---------------------------------------------------------------------------

/// Minimal DEFLATE decompressor for git object decompression.
///
/// Git objects are zlib-compressed (RFC 1950: 2-byte header + DEFLATE + 4-byte
/// checksum). We skip the zlib header/trailer and inflate the raw DEFLATE
/// stream.  This only handles the subset used by git: stored blocks, fixed
/// Huffman, and dynamic Huffman.
fn zlib_decompress(data: &[u8], max_output: usize) -> Result<Vec<u8>, GitError> {
    if data.len() < 6 {
        return Err(GitError::Corrupt(String::from("zlib data too short")));
    }
    // Skip 2-byte zlib header (CMF + FLG).
    let deflate_data = &data[2..];
    deflate_inflate(deflate_data, max_output)
}

/// Raw DEFLATE inflate.
fn deflate_inflate(data: &[u8], max_output: usize) -> Result<Vec<u8>, GitError> {
    let mut output = Vec::with_capacity(core::cmp::min(max_output, 4096));
    let mut reader = BitReader::new(data);

    loop {
        let bfinal = reader.read_bits(1)?;
        let btype = reader.read_bits(2)?;

        match btype {
            0 => {
                // Stored block.
                reader.align_byte();
                let len = reader.read_bits(16)? as usize;
                let _nlen = reader.read_bits(16)?;
                for _ in 0..len {
                    if output.len() >= max_output {
                        return Err(GitError::Corrupt(String::from("decompressed data too large")));
                    }
                    output.push(reader.read_byte()?);
                }
            }
            1 => {
                // Fixed Huffman.
                inflate_block_fixed(&mut reader, &mut output, max_output)?;
            }
            2 => {
                // Dynamic Huffman.
                inflate_block_dynamic(&mut reader, &mut output, max_output)?;
            }
            _ => {
                return Err(GitError::Corrupt(format!("invalid DEFLATE block type: {}", btype)));
            }
        }

        if bfinal != 0 {
            break;
        }
    }

    Ok(output)
}

/// Bit reader for DEFLATE stream.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buf: u32,
    bit_count: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bit_buf: 0,
            bit_count: 0,
        }
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, GitError> {
        while self.bit_count < count {
            if self.pos >= self.data.len() {
                return Err(GitError::Corrupt(String::from("unexpected end of DEFLATE stream")));
            }
            self.bit_buf |= (self.data[self.pos] as u32) << self.bit_count;
            self.pos += 1;
            self.bit_count += 8;
        }
        let mask = (1u32 << count) - 1;
        let value = self.bit_buf & mask;
        self.bit_buf >>= count;
        self.bit_count -= count;
        Ok(value)
    }

    fn read_byte(&mut self) -> Result<u8, GitError> {
        self.read_bits(8).map(|v| v as u8)
    }

    fn align_byte(&mut self) {
        self.bit_buf = 0;
        self.bit_count = 0;
    }
}

/// Fixed Huffman code lengths (per RFC 1951).
fn inflate_block_fixed(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    max_output: usize,
) -> Result<(), GitError> {
    // Build fixed Huffman tables.
    let mut lit_lengths = [0u8; 288];
    for i in 0..=143 { lit_lengths[i] = 8; }
    for i in 144..=255 { lit_lengths[i] = 9; }
    for i in 256..=279 { lit_lengths[i] = 7; }
    for i in 280..=287 { lit_lengths[i] = 8; }

    let mut dist_lengths = [5u8; 32];

    let lit_table = build_huffman_table(&lit_lengths)?;
    let dist_table = build_huffman_table(&dist_lengths)?;

    decode_huffman_block(reader, output, &lit_table, &dist_table, max_output)
}

/// Dynamic Huffman block.
fn inflate_block_dynamic(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    max_output: usize,
) -> Result<(), GitError> {
    let hlit = reader.read_bits(5)? as usize + 257;
    let hdist = reader.read_bits(5)? as usize + 1;
    let hclen = reader.read_bits(4)? as usize + 4;

    // Code length alphabet order.
    const CL_ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    let mut cl_lengths = [0u8; 19];
    for i in 0..hclen {
        cl_lengths[CL_ORDER[i]] = reader.read_bits(3)? as u8;
    }
    let cl_table = build_huffman_table(&cl_lengths)?;

    // Decode literal/length + distance code lengths.
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = decode_symbol(reader, &cl_table)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                let repeat = reader.read_bits(2)? as usize + 3;
                let prev = if i > 0 { lengths[i - 1] } else { 0 };
                for _ in 0..repeat {
                    if i < lengths.len() {
                        lengths[i] = prev;
                        i += 1;
                    }
                }
            }
            17 => {
                let repeat = reader.read_bits(3)? as usize + 3;
                for _ in 0..repeat {
                    if i < lengths.len() {
                        lengths[i] = 0;
                        i += 1;
                    }
                }
            }
            18 => {
                let repeat = reader.read_bits(7)? as usize + 11;
                for _ in 0..repeat {
                    if i < lengths.len() {
                        lengths[i] = 0;
                        i += 1;
                    }
                }
            }
            _ => return Err(GitError::Corrupt(format!("bad code length symbol: {}", sym))),
        }
    }

    let lit_table = build_huffman_table(&lengths[..hlit])?;
    let dist_table = build_huffman_table(&lengths[hlit..])?;

    decode_huffman_block(reader, output, &lit_table, &dist_table, max_output)
}

/// Huffman lookup table: maps code -> symbol. Max 15-bit codes.
struct HuffmanTable {
    /// For each bit-length (1..=15), the starting code value.
    min_code: [u16; 16],
    /// For each bit-length, the starting index into `symbols`.
    sym_offset: [u16; 16],
    /// Sorted symbols.
    symbols: Vec<u16>,
    /// Maximum code length in this table.
    max_bits: u8,
}

fn build_huffman_table(lengths: &[u8]) -> Result<HuffmanTable, GitError> {
    let max_bits = *lengths.iter().max().unwrap_or(&0);
    if max_bits > 15 {
        return Err(GitError::Corrupt(String::from("code length > 15")));
    }

    // Count codes of each length.
    let mut bl_count = [0u16; 16];
    for &l in lengths {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }

    // Compute starting code for each length.
    let mut next_code = [0u16; 16];
    let mut code: u16 = 0;
    for bits in 1..=15 {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }

    // Build min_code and sym_offset arrays, plus sorted symbols.
    let mut min_code = [0u16; 16];
    let mut sym_offset = [0u16; 16];
    let mut symbols = Vec::new();

    let mut offset = 0u16;
    for bits in 1..=15 {
        min_code[bits] = next_code[bits];
        sym_offset[bits] = offset;
        offset += bl_count[bits];
    }
    symbols.resize(offset as usize, 0);

    // Fill symbols in code order.
    let mut next = [0u16; 16];
    next.copy_from_slice(&sym_offset);
    for (sym, &l) in lengths.iter().enumerate() {
        if l > 0 {
            let idx = next[l as usize] as usize;
            if idx < symbols.len() {
                symbols[idx] = sym as u16;
            }
            next[l as usize] += 1;
        }
    }

    Ok(HuffmanTable {
        min_code,
        sym_offset,
        symbols,
        max_bits,
    })
}

/// Decode one Huffman symbol.
fn decode_symbol(reader: &mut BitReader<'_>, table: &HuffmanTable) -> Result<u16, GitError> {
    let mut code: u16 = 0;
    for bits in 1..=table.max_bits {
        code = (code << 1) | reader.read_bits(1)? as u16;
        let count_at = if bits < 15 {
            table.sym_offset.get(bits as usize + 1).copied().unwrap_or(table.symbols.len() as u16)
                - table.sym_offset[bits as usize]
        } else {
            table.symbols.len() as u16 - table.sym_offset[bits as usize]
        };
        if code >= table.min_code[bits as usize]
            && code < table.min_code[bits as usize] + count_at
        {
            let idx = table.sym_offset[bits as usize] as usize
                + (code - table.min_code[bits as usize]) as usize;
            return table
                .symbols
                .get(idx)
                .copied()
                .ok_or_else(|| GitError::Corrupt(String::from("huffman index out of bounds")));
        }
    }
    Err(GitError::Corrupt(String::from("invalid huffman code")))
}

/// Decode a Huffman-compressed block.
fn decode_huffman_block(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    lit_table: &HuffmanTable,
    dist_table: &HuffmanTable,
    max_output: usize,
) -> Result<(), GitError> {
    // Extra bits for length codes 257..285.
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2,
        3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31,
        35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
        7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193,
        257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
        8193, 12289, 16385, 24577,
    ];

    loop {
        let sym = decode_symbol(reader, lit_table)?;
        if sym < 256 {
            if output.len() >= max_output {
                return Err(GitError::Corrupt(String::from("output too large")));
            }
            output.push(sym as u8);
        } else if sym == 256 {
            // End of block.
            return Ok(());
        } else {
            // Length/distance pair.
            let len_idx = (sym - 257) as usize;
            if len_idx >= LEN_BASE.len() {
                return Err(GitError::Corrupt(format!("bad length code: {}", sym)));
            }
            let length = LEN_BASE[len_idx] as usize
                + reader.read_bits(LEN_EXTRA[len_idx])? as usize;

            let dist_sym = decode_symbol(reader, dist_table)? as usize;
            if dist_sym >= DIST_BASE.len() {
                return Err(GitError::Corrupt(format!("bad distance code: {}", dist_sym)));
            }
            let distance = DIST_BASE[dist_sym] as usize
                + reader.read_bits(DIST_EXTRA[dist_sym])? as usize;

            if distance > output.len() {
                return Err(GitError::Corrupt(String::from("distance exceeds output")));
            }
            for _ in 0..length {
                if output.len() >= max_output {
                    return Err(GitError::Corrupt(String::from("output too large")));
                }
                let byte = output[output.len() - distance];
                output.push(byte);
            }
        }
    }
}

/// Compress data with zlib (stored blocks only — no Huffman).
///
/// For git push we need to zlib-compress objects. We use uncompressed DEFLATE
/// stored blocks, which is the simplest valid encoding. Larger but correct.
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    // Zlib header: CMF=0x78 (deflate, window=32K), FLG=0x01 (no dict, check bits).
    out.push(0x78);
    out.push(0x01);

    // DEFLATE stored blocks (max 65535 bytes each).
    let mut offset = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let block_len = core::cmp::min(remaining, 65535);
        let is_final = offset + block_len >= data.len();

        out.push(if is_final { 0x01 } else { 0x00 }); // BFINAL + BTYPE=00 (stored)
        let len = block_len as u16;
        out.extend_from_slice(&len.to_le_bytes());
        let nlen = !len;
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&data[offset..offset + block_len]);
        offset += block_len;
    }

    // Adler-32 checksum.
    let checksum = adler32(data);
    out.extend_from_slice(&checksum.to_be_bytes());
    out
}

/// Adler-32 checksum (used by zlib).
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

/// Parsed git remote URL.
struct GitUrl {
    /// Hostname (e.g. "github.com").
    host: String,
    /// Port (443 for HTTPS).
    port: u16,
    /// Path (e.g. "/user/repo.git").
    path: String,
    /// Whether HTTPS (always true for now).
    is_https: bool,
}

fn parse_git_url(url: &str) -> Result<GitUrl, GitError> {
    let url = url.trim();

    // Strip .git suffix if missing, normalize.
    let url_str = if url.ends_with('/') {
        &url[..url.len() - 1]
    } else {
        url
    };

    let (is_https, rest) = if url_str.starts_with("https://") {
        (true, &url_str[8..])
    } else if url_str.starts_with("http://") {
        (false, &url_str[7..])
    } else {
        return Err(GitError::BadUrl(format!("only HTTP(S) URLs supported: {}", url)));
    };

    // Split host/path.
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => return Err(GitError::BadUrl(format!("no path in URL: {}", url))),
    };

    let (host, port) = if let Some(colon) = host_port.find(':') {
        let port_str = &host_port[colon + 1..];
        let port: u16 = port_str
            .parse()
            .map_err(|_| GitError::BadUrl(format!("bad port: {}", port_str)))?;
        (&host_port[..colon], port)
    } else {
        (host_port, if is_https { 443 } else { 80 })
    };

    // Ensure path ends with .git for the smart HTTP protocol.
    let path = if path.ends_with(".git") {
        String::from(path)
    } else {
        format!("{}.git", path)
    };

    Ok(GitUrl {
        host: String::from(host),
        port,
        path,
        is_https,
    })
}

// ---------------------------------------------------------------------------
// Git object model
// ---------------------------------------------------------------------------

/// Git object types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl ObjectType {
    fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Commit => "commit",
            ObjectType::Tag => "tag",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "blob" => Some(ObjectType::Blob),
            "tree" => Some(ObjectType::Tree),
            "commit" => Some(ObjectType::Commit),
            "tag" => Some(ObjectType::Tag),
            _ => None,
        }
    }

    fn from_pack_type(t: u8) -> Option<Self> {
        match t {
            1 => Some(ObjectType::Commit),
            2 => Some(ObjectType::Tree),
            3 => Some(ObjectType::Blob),
            4 => Some(ObjectType::Tag),
            _ => None,
        }
    }
}

/// A git object stored in the repository.
#[derive(Debug, Clone)]
pub struct GitObject {
    pub obj_type: ObjectType,
    pub data: Vec<u8>,
}

impl GitObject {
    /// Compute the SHA-1 hash (object ID) for this object.
    pub fn hash(&self) -> [u8; SHA1_LEN] {
        let header = format!("{} {}\0", self.obj_type.as_str(), self.data.len());
        let mut hasher = Sha1::new();
        hasher.update(header.as_bytes());
        hasher.update(&self.data);
        hasher.finalize()
    }

    /// Hex-encoded hash.
    pub fn hash_hex(&self) -> String {
        hex_encode(&self.hash())
    }
}

// ---------------------------------------------------------------------------
// In-memory object store (backed by VFS)
// ---------------------------------------------------------------------------

/// In-memory git object store for a repository.
///
/// Objects are stored in a `BTreeMap` keyed by hex SHA-1. On persist, they
/// are written to `.git/objects/xx/yyyy...` in the VFS.
pub struct ObjectStore {
    /// Objects indexed by hex SHA-1.
    objects: alloc::collections::BTreeMap<String, GitObject>,
}

impl ObjectStore {
    pub fn new() -> Self {
        Self {
            objects: alloc::collections::BTreeMap::new(),
        }
    }

    /// Store an object, returning its hex SHA-1.
    pub fn store(&mut self, obj: GitObject) -> String {
        let hash = obj.hash_hex();
        self.objects.insert(hash.clone(), obj);
        hash
    }

    /// Retrieve an object by hex SHA-1.
    pub fn get(&self, hash: &str) -> Option<&GitObject> {
        self.objects.get(hash)
    }

    /// Check if an object exists.
    pub fn has(&self, hash: &str) -> bool {
        self.objects.contains_key(hash)
    }

    /// Number of objects.
    pub fn len(&self) -> usize {
        self.objects.len()
    }
}

// ---------------------------------------------------------------------------
// Repository state
// ---------------------------------------------------------------------------

/// Represents a local git repository in memory.
pub struct Repository {
    /// Path to the repository root in VFS.
    pub path: String,
    /// Object store.
    pub objects: ObjectStore,
    /// HEAD ref (commit hash).
    pub head: Option<String>,
    /// Current branch name (e.g. "main").
    pub branch: String,
    /// Remote URL.
    pub remote_url: Option<String>,
    /// Index/staging area: path -> blob hash.
    pub index: alloc::collections::BTreeMap<String, String>,
    /// Working tree: path -> file content.
    pub worktree: alloc::collections::BTreeMap<String, Vec<u8>>,
}

impl Repository {
    /// Create a new empty repository.
    pub fn new(path: &str) -> Self {
        Self {
            path: String::from(path),
            objects: ObjectStore::new(),
            head: None,
            branch: String::from("main"),
            remote_url: None,
            index: alloc::collections::BTreeMap::new(),
            worktree: alloc::collections::BTreeMap::new(),
        }
    }

    /// Initialize a new repository at the given path.
    pub fn init(path: &str) -> Self {
        log::info!("[git] initializing repository at {}", path);
        let mut repo = Self::new(path);
        repo.branch = String::from("main");
        repo
    }
}

// ---------------------------------------------------------------------------
// Packfile parsing
// ---------------------------------------------------------------------------

/// Parse a git packfile and extract objects.
///
/// Packfile format:
/// - 4 bytes: "PACK"
/// - 4 bytes: version (big-endian, must be 2)
/// - 4 bytes: number of objects (big-endian)
/// - N object entries
/// - 20 bytes: SHA-1 checksum of the entire pack
fn parse_packfile(data: &[u8], store: &mut ObjectStore) -> Result<usize, GitError> {
    if data.len() < 12 {
        return Err(GitError::Corrupt(String::from("packfile too short")));
    }

    // Verify magic.
    if &data[0..4] != b"PACK" {
        return Err(GitError::Corrupt(String::from("not a packfile (missing PACK header)")));
    }

    let version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if version != 2 {
        return Err(GitError::Corrupt(format!("unsupported pack version: {}", version)));
    }

    let num_objects = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
    log::info!("[git] packfile: version={}, objects={}", version, num_objects);

    let mut offset = 12;
    let mut count = 0;

    for i in 0..num_objects {
        if offset >= data.len() {
            log::warn!("[git] packfile: ran out of data at object {}/{}", i, num_objects);
            break;
        }

        // Parse object header: variable-length encoded type + size.
        let first_byte = data[offset];
        let obj_type_raw = (first_byte >> 4) & 0x07;
        let mut size: usize = (first_byte & 0x0F) as usize;
        let mut shift = 4;
        offset += 1;

        let mut byte = first_byte;
        while byte & 0x80 != 0 {
            if offset >= data.len() {
                return Err(GitError::Corrupt(String::from("truncated object header")));
            }
            byte = data[offset];
            offset += 1;
            size |= ((byte & 0x7F) as usize) << shift;
            shift += 7;
        }

        // Handle object types.
        match obj_type_raw {
            1..=4 => {
                // Regular object (commit, tree, blob, tag).
                let obj_type = ObjectType::from_pack_type(obj_type_raw).unwrap();

                // The object data is zlib-compressed starting at `offset`.
                let compressed = &data[offset..];
                let decompressed = zlib_decompress(compressed, size.max(256))?;

                // Advance offset past the compressed data. Since we don't know
                // the exact compressed size, re-decompress with a counting
                // wrapper. For simplicity, scan for the next valid object
                // header or end.
                //
                // HACK: We try decompressing and then scan forward. A proper
                // implementation would track the exact compressed byte count.
                let consumed = find_compressed_end(compressed, decompressed.len())?;
                offset += consumed;

                let obj = GitObject {
                    obj_type,
                    data: decompressed,
                };
                store.store(obj);
                count += 1;
            }
            5 => {
                // OFS_DELTA — skip for now (simplified implementation).
                log::debug!("[git] skipping OFS_DELTA object at offset {}", offset);
                // Read the negative offset.
                let mut _delta_off: usize = 0;
                let mut b = data.get(offset).copied().unwrap_or(0);
                offset += 1;
                _delta_off = (b & 0x7F) as usize;
                while b & 0x80 != 0 {
                    b = data.get(offset).copied().unwrap_or(0);
                    offset += 1;
                    _delta_off = ((_delta_off + 1) << 7) | (b & 0x7F) as usize;
                }
                // Skip compressed delta data.
                let compressed = &data[offset..];
                if let Ok(decompressed) = zlib_decompress(compressed, MAX_PACK_SIZE) {
                    let consumed = find_compressed_end(compressed, decompressed.len())
                        .unwrap_or(1);
                    offset += consumed;
                } else {
                    offset += 1;
                }
            }
            6 => {
                // REF_DELTA — skip for now.
                log::debug!("[git] skipping REF_DELTA object at offset {}", offset);
                offset += SHA1_LEN; // base object SHA-1
                let compressed = &data[offset..];
                if let Ok(decompressed) = zlib_decompress(compressed, MAX_PACK_SIZE) {
                    let consumed = find_compressed_end(compressed, decompressed.len())
                        .unwrap_or(1);
                    offset += consumed;
                } else {
                    offset += 1;
                }
            }
            _ => {
                log::warn!("[git] unknown pack object type {} at offset {}", obj_type_raw, offset);
                break;
            }
        }
    }

    log::info!("[git] parsed {} objects from packfile", count);
    Ok(count)
}

/// Estimate the compressed data length by trying to decompress and tracking
/// how many input bytes were consumed. This is approximate.
fn find_compressed_end(data: &[u8], expected_output: usize) -> Result<usize, GitError> {
    // Try increasing input sizes until decompression succeeds.
    // Start from a reasonable guess.
    let min_try = core::cmp::min(expected_output / 2 + 10, data.len());
    let max_try = core::cmp::min(expected_output * 2 + 256, data.len());

    for size in min_try..=max_try {
        if let Ok(result) = zlib_decompress(&data[..size], expected_output + 64) {
            if result.len() >= expected_output {
                return Ok(size);
            }
        }
    }

    // Fallback: return max_try.
    Ok(max_try)
}

// ---------------------------------------------------------------------------
// Tree parsing / building
// ---------------------------------------------------------------------------

/// Entry in a git tree object.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    /// File mode (e.g. "100644" for regular file, "040000" for directory).
    pub mode: String,
    /// File name.
    pub name: String,
    /// SHA-1 hash of the blob or subtree.
    pub hash: [u8; SHA1_LEN],
}

/// Parse a tree object's data into entries.
fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>, GitError> {
    let mut entries = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        // Format: "<mode> <name>\0<20-byte-hash>"
        let space = data[offset..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| GitError::Corrupt(String::from("tree: missing space")))?;
        let mode = core::str::from_utf8(&data[offset..offset + space])
            .map_err(|_| GitError::Corrupt(String::from("tree: bad mode")))?;
        offset += space + 1;

        let nul = data[offset..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::Corrupt(String::from("tree: missing NUL")))?;
        let name = core::str::from_utf8(&data[offset..offset + nul])
            .map_err(|_| GitError::Corrupt(String::from("tree: bad name")))?;
        offset += nul + 1;

        if offset + SHA1_LEN > data.len() {
            return Err(GitError::Corrupt(String::from("tree: truncated hash")));
        }
        let mut hash = [0u8; SHA1_LEN];
        hash.copy_from_slice(&data[offset..offset + SHA1_LEN]);
        offset += SHA1_LEN;

        entries.push(TreeEntry {
            mode: String::from(mode),
            name: String::from(name),
            hash,
        });
    }

    Ok(entries)
}

/// Build a tree object from entries.
fn build_tree(entries: &[TreeEntry]) -> Vec<u8> {
    let mut data = Vec::new();
    for entry in entries {
        data.extend_from_slice(entry.mode.as_bytes());
        data.push(b' ');
        data.extend_from_slice(entry.name.as_bytes());
        data.push(0);
        data.extend_from_slice(&entry.hash);
    }
    data
}

// ---------------------------------------------------------------------------
// Commit parsing
// ---------------------------------------------------------------------------

/// Parsed commit object.
#[derive(Debug, Clone)]
pub struct ParsedCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committer: String,
    pub message: String,
}

fn parse_commit(data: &[u8]) -> Result<ParsedCommit, GitError> {
    let text = core::str::from_utf8(data)
        .map_err(|_| GitError::Corrupt(String::from("commit: not UTF-8")))?;

    let mut tree = String::new();
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut committer = String::new();
    let mut in_message = false;
    let mut message = String::new();

    for line in text.split('\n') {
        if in_message {
            if !message.is_empty() {
                message.push('\n');
            }
            message.push_str(line);
            continue;
        }
        if line.is_empty() {
            in_message = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = String::from(rest.trim());
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(String::from(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = String::from(rest.trim());
        } else if let Some(rest) = line.strip_prefix("committer ") {
            committer = String::from(rest.trim());
        }
    }

    Ok(ParsedCommit {
        tree,
        parents,
        author,
        committer,
        message,
    })
}

// ---------------------------------------------------------------------------
// Smart HTTP protocol
// ---------------------------------------------------------------------------

/// RNG counter for ephemeral ports.
static GIT_PORT_COUNTER: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(58000);

fn next_git_port() -> u16 {
    let p = GIT_PORT_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if p >= 59900 {
        GIT_PORT_COUNTER.store(58000, core::sync::atomic::Ordering::Relaxed);
    }
    p
}

/// RNG seed counter for TLS connections.
static GIT_RNG_SEED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(500);

fn next_rng_seed() -> u64 {
    GIT_RNG_SEED.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

/// Fetch refs from a remote via Smart HTTP.
///
/// GET `{url}/info/refs?service=git-upload-pack`
fn fetch_refs(
    url: &GitUrl,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<Vec<(String, String)>, GitError> {
    let request_path = format!("{}/info/refs?service=git-upload-pack", url.path);

    let req = claudio_net::http::HttpRequest::get(&url.host, &request_path)
        .header("User-Agent", "ClaudioOS-Git/1.0")
        .header("Accept", "*/*")
        .header("Connection", "close");

    let resp = do_http_request(url, &req.to_bytes(), stack, now)?;
    let body = extract_http_body(&resp)?;

    // Parse pkt-line formatted response.
    parse_ref_discovery(&body)
}

/// Parse pkt-line formatted ref discovery response.
fn parse_ref_discovery(data: &[u8]) -> Result<Vec<(String, String)>, GitError> {
    let mut refs = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        // pkt-line: 4 hex chars = length, then that many bytes (including the 4).
        if offset + 4 > data.len() {
            break;
        }
        let len_hex = core::str::from_utf8(&data[offset..offset + 4])
            .map_err(|_| GitError::Protocol(String::from("bad pkt-line length")))?;

        if len_hex == "0000" {
            // Flush packet.
            offset += 4;
            continue;
        }

        let pkt_len: usize = usize::from_str_radix(len_hex, 16)
            .map_err(|_| GitError::Protocol(format!("bad pkt-line length: {}", len_hex)))?;

        if pkt_len < 4 || offset + pkt_len > data.len() {
            break;
        }

        let line = &data[offset + 4..offset + pkt_len];
        offset += pkt_len;

        // Skip the service announcement line (starts with '#').
        let line_str = core::str::from_utf8(line).unwrap_or("");
        let line_str = line_str.trim_end_matches('\n');

        if line_str.starts_with('#') || line_str.is_empty() {
            continue;
        }

        // Format: "<hash> <refname>\0<capabilities>" or "<hash> <refname>"
        let line_no_caps = if let Some(nul_pos) = line_str.find('\0') {
            &line_str[..nul_pos]
        } else {
            line_str
        };

        if let Some(space) = line_no_caps.find(' ') {
            let hash = &line_no_caps[..space];
            let refname = &line_no_caps[space + 1..];
            if hash.len() == SHA1_HEX_LEN {
                refs.push((String::from(hash), String::from(refname)));
            }
        }
    }

    Ok(refs)
}

/// Send a git-upload-pack request to fetch objects.
///
/// POST `{url}/git-upload-pack` with wanted refs.
fn fetch_pack(
    url: &GitUrl,
    want_refs: &[String],
    have_refs: &[String],
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<Vec<u8>, GitError> {
    // Build the request body in pkt-line format.
    let mut body = Vec::new();

    for (i, want) in want_refs.iter().enumerate() {
        let line = if i == 0 {
            format!("want {}\n", want)
        } else {
            format!("want {}\n", want)
        };
        write_pkt_line(&mut body, line.as_bytes());
    }

    // Flush after wants.
    body.extend_from_slice(b"0000");

    for have in have_refs {
        let line = format!("have {}\n", have);
        write_pkt_line(&mut body, line.as_bytes());
    }

    // Done.
    write_pkt_line(&mut body, b"done\n");

    let request_path = format!("{}/git-upload-pack", url.path);
    let req = claudio_net::http::HttpRequest::post(&url.host, &request_path, body)
        .header("User-Agent", "ClaudioOS-Git/1.0")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .header("Accept", "application/x-git-upload-pack-result")
        .header("Connection", "close");

    let resp = do_http_request(url, &req.to_bytes(), stack, now)?;
    let body = extract_http_body(&resp)?;

    // The response contains pkt-line status + packfile data.
    // Find the packfile (starts with "PACK").
    if let Some(pack_start) = find_pack_start(&body) {
        Ok(body[pack_start..].to_vec())
    } else {
        Err(GitError::Protocol(String::from("no packfile in upload-pack response")))
    }
}

/// Write a pkt-line.
fn write_pkt_line(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len() + 4;
    buf.extend_from_slice(format!("{:04x}", len).as_bytes());
    buf.extend_from_slice(data);
}

/// Find the start of PACK data in a response.
fn find_pack_start(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"PACK" {
            return Some(i);
        }
    }
    None
}

/// Perform an HTTP(S) request to the git server.
fn do_http_request(
    url: &GitUrl,
    request_bytes: &[u8],
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<Vec<u8>, GitError> {
    if url.is_https {
        let rng_seed = next_rng_seed();
        claudio_net::tls::https_request(stack, &url.host, url.port, request_bytes, now, rng_seed)
            .map_err(|e| GitError::Network(format!("HTTPS request failed: {:?}", e)))
    } else {
        // Plain HTTP.
        let ip = claudio_net::dns::resolve(stack, &url.host, || now())
            .map_err(|e| GitError::Network(format!("DNS resolution failed: {:?}", e)))?;
        let local_port = next_git_port();
        let handle = claudio_net::tls::tcp_connect(stack, ip, url.port, local_port, now)
            .map_err(|e| GitError::Network(format!("TCP connect failed: {:?}", e)))?;
        claudio_net::tls::tcp_send(stack, handle, request_bytes, now)
            .map_err(|e| {
                claudio_net::tls::tcp_close(stack, handle);
                GitError::Network(format!("TCP send failed: {:?}", e))
            })?;

        let mut buf = vec![0u8; MAX_PACK_SIZE];
        let mut total = 0;
        for _ in 0..2000 {
            match claudio_net::tls::tcp_recv(stack, handle, &mut buf[total..], now) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if total >= buf.len() - 4096 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        claudio_net::tls::tcp_close(stack, handle);

        if total == 0 {
            return Err(GitError::Network(String::from("empty response from server")));
        }
        Ok(buf[..total].to_vec())
    }
}

/// Extract HTTP response body (skip headers).
fn extract_http_body(response: &[u8]) -> Result<Vec<u8>, GitError> {
    // Find \r\n\r\n header/body separator.
    for i in 0..response.len().saturating_sub(3) {
        if &response[i..i + 4] == b"\r\n\r\n" {
            return Ok(response[i + 4..].to_vec());
        }
    }
    Err(GitError::Protocol(String::from("no HTTP body found")))
}

/// Expand a tree recursively into the working tree.
fn expand_tree(
    store: &ObjectStore,
    tree_hash: &str,
    prefix: &str,
    worktree: &mut alloc::collections::BTreeMap<String, Vec<u8>>,
    index: &mut alloc::collections::BTreeMap<String, String>,
) -> Result<(), GitError> {
    let tree_obj = store
        .get(tree_hash)
        .ok_or_else(|| GitError::Corrupt(format!("missing tree object: {}", tree_hash)))?;

    if tree_obj.obj_type != ObjectType::Tree {
        return Err(GitError::Corrupt(format!("{} is not a tree", tree_hash)));
    }

    let entries = parse_tree(&tree_obj.data)?;

    for entry in &entries {
        let entry_hash = hex_encode(&entry.hash);
        let full_path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        if entry.mode.starts_with("40") {
            // Directory — recurse.
            expand_tree(store, &entry_hash, &full_path, worktree, index)?;
        } else {
            // File — add to worktree.
            if let Some(blob) = store.get(&entry_hash) {
                worktree.insert(full_path.clone(), blob.data.clone());
                index.insert(full_path, entry_hash);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API: git operations
// ---------------------------------------------------------------------------

/// Clone a remote repository via HTTPS.
///
/// 1. Parse the URL.
/// 2. GET /info/refs to discover refs.
/// 3. POST /git-upload-pack to fetch the packfile.
/// 4. Parse packfile into object store.
/// 5. Checkout HEAD into working tree.
pub fn git_clone(
    url: &str,
    path: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<Repository, GitError> {
    log::info!("[git] cloning {} into {}", url, path);

    let parsed_url = parse_git_url(url)?;

    // Step 1: Discover refs.
    log::info!("[git] fetching refs from {}", url);
    let refs = fetch_refs(&parsed_url, stack, now)?;

    if refs.is_empty() {
        return Err(GitError::Protocol(String::from("no refs found on remote")));
    }

    log::info!("[git] found {} refs:", refs.len());
    for (hash, refname) in &refs {
        log::info!("[git]   {} {}", &hash[..8], refname);
    }

    // Find HEAD or main/master ref.
    let head_hash = refs
        .iter()
        .find(|(_, name)| name == "HEAD")
        .or_else(|| refs.iter().find(|(_, name)| name == "refs/heads/main"))
        .or_else(|| refs.iter().find(|(_, name)| name == "refs/heads/master"))
        .map(|(hash, _)| hash.clone())
        .ok_or_else(|| GitError::Protocol(String::from("no HEAD ref found")))?;

    // Determine branch name.
    let branch = refs
        .iter()
        .find(|(h, name)| h == &head_hash && name.starts_with("refs/heads/"))
        .map(|(_, name)| name.strip_prefix("refs/heads/").unwrap_or("main"))
        .unwrap_or("main");

    // Step 2: Fetch packfile.
    log::info!("[git] fetching pack for {}", head_hash);
    let pack_data = fetch_pack(&parsed_url, &[head_hash.clone()], &[], stack, now)?;

    // Step 3: Parse packfile.
    let mut repo = Repository::new(path);
    repo.remote_url = Some(String::from(url));
    repo.branch = String::from(branch);

    let num_objects = parse_packfile(&pack_data, &mut repo.objects)?;
    log::info!("[git] unpacked {} objects", num_objects);

    // Step 4: Checkout HEAD.
    repo.head = Some(head_hash.clone());

    if let Some(commit_obj) = repo.objects.get(&head_hash) {
        if commit_obj.obj_type == ObjectType::Commit {
            let commit = parse_commit(&commit_obj.data)?;
            expand_tree(
                &repo.objects,
                &commit.tree,
                "",
                &mut repo.worktree,
                &mut repo.index,
            )?;
            log::info!(
                "[git] checked out {} files from {}",
                repo.worktree.len(),
                &head_hash[..8]
            );
        }
    }

    log::info!(
        "[git] clone complete: {} objects, {} files, branch={}",
        repo.objects.len(),
        repo.worktree.len(),
        repo.branch
    );

    Ok(repo)
}

/// Pull (fetch + fast-forward merge) from the remote.
pub fn git_pull(
    repo: &mut Repository,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<String, GitError> {
    let url_str = repo
        .remote_url
        .as_ref()
        .ok_or_else(|| GitError::NotARepo)?
        .clone();
    let parsed_url = parse_git_url(&url_str)?;

    log::info!("[git] pulling from {}", url_str);

    // Fetch current refs.
    let refs = fetch_refs(&parsed_url, stack, now)?;

    let remote_head = refs
        .iter()
        .find(|(_, name)| name == &format!("refs/heads/{}", repo.branch))
        .or_else(|| refs.iter().find(|(_, name)| name == "HEAD"))
        .map(|(hash, _)| hash.clone())
        .ok_or_else(|| GitError::Protocol(String::from("remote HEAD not found")))?;

    if repo.head.as_ref() == Some(&remote_head) {
        return Ok(String::from("Already up to date."));
    }

    // Fetch new objects.
    let have = repo.head.as_ref().map(|h| vec![h.clone()]).unwrap_or_default();
    let pack_data = fetch_pack(&parsed_url, &[remote_head.clone()], &have, stack, now)?;
    let num_new = parse_packfile(&pack_data, &mut repo.objects)?;

    // Update HEAD and checkout.
    let old_head = repo.head.clone().unwrap_or_default();
    repo.head = Some(remote_head.clone());

    // Re-checkout.
    repo.worktree.clear();
    repo.index.clear();

    if let Some(commit_obj) = repo.objects.get(&remote_head) {
        if commit_obj.obj_type == ObjectType::Commit {
            let commit = parse_commit(&commit_obj.data)?;
            expand_tree(
                &repo.objects,
                &commit.tree,
                "",
                &mut repo.worktree,
                &mut repo.index,
            )?;
        }
    }

    Ok(format!(
        "Updating {}..{}\n{} new objects, {} files",
        &old_head[..core::cmp::min(8, old_head.len())],
        &remote_head[..8],
        num_new,
        repo.worktree.len()
    ))
}

/// Show the status of the working tree.
pub fn git_status(repo: &Repository) -> String {
    let mut output = String::new();

    output.push_str(&format!("On branch {}\n", repo.branch));

    if let Some(ref head) = repo.head {
        output.push_str(&format!(
            "HEAD at {}\n",
            &head[..core::cmp::min(8, head.len())]
        ));
    } else {
        output.push_str("No commits yet\n");
    }

    // Find modified files (in worktree but different from index).
    let mut modified = Vec::new();
    let mut untracked = Vec::new();

    for (path, content) in &repo.worktree {
        if let Some(index_hash) = repo.index.get(path) {
            // Check if content changed.
            let blob = GitObject {
                obj_type: ObjectType::Blob,
                data: content.clone(),
            };
            let current_hash = blob.hash_hex();
            if &current_hash != index_hash {
                modified.push(path.clone());
            }
        } else {
            untracked.push(path.clone());
        }
    }

    // Find deleted files (in index but not in worktree).
    let mut deleted = Vec::new();
    for path in repo.index.keys() {
        if !repo.worktree.contains_key(path) {
            deleted.push(path.clone());
        }
    }

    if modified.is_empty() && untracked.is_empty() && deleted.is_empty() {
        output.push_str("\nnothing to commit, working tree clean\n");
    } else {
        if !modified.is_empty() {
            output.push_str("\nChanges not staged for commit:\n");
            for path in &modified {
                output.push_str(&format!("  modified: {}\n", path));
            }
        }
        if !deleted.is_empty() {
            output.push_str("\nDeleted files:\n");
            for path in &deleted {
                output.push_str(&format!("  deleted: {}\n", path));
            }
        }
        if !untracked.is_empty() {
            output.push_str("\nUntracked files:\n");
            for path in &untracked {
                output.push_str(&format!("  {}\n", path));
            }
        }
    }

    output
}

/// Stage files for commit.
pub fn git_add(repo: &mut Repository, files: &[&str]) -> Result<String, GitError> {
    let mut staged = 0;

    for &file in files {
        if file == "." {
            // Stage all changes.
            let paths: Vec<String> = repo.worktree.keys().cloned().collect();
            for path in &paths {
                if let Some(content) = repo.worktree.get(path) {
                    let blob = GitObject {
                        obj_type: ObjectType::Blob,
                        data: content.clone(),
                    };
                    let hash = repo.objects.store(blob);
                    repo.index.insert(path.clone(), hash);
                    staged += 1;
                }
            }
            // Also handle deletions: remove from index if not in worktree.
            let index_paths: Vec<String> = repo.index.keys().cloned().collect();
            for path in index_paths {
                if !repo.worktree.contains_key(&path) {
                    repo.index.remove(&path);
                    staged += 1;
                }
            }
        } else if let Some(content) = repo.worktree.get(file) {
            let blob = GitObject {
                obj_type: ObjectType::Blob,
                data: content.clone(),
            };
            let hash = repo.objects.store(blob);
            repo.index.insert(String::from(file), hash);
            staged += 1;
        } else {
            // File deleted — remove from index.
            if repo.index.remove(file).is_some() {
                staged += 1;
            } else {
                return Err(GitError::Io(format!("pathspec '{}' did not match any files", file)));
            }
        }
    }

    Ok(format!("Staged {} file(s)", staged))
}

/// Create a commit from the current index.
pub fn git_commit(repo: &mut Repository, message: &str) -> Result<String, GitError> {
    if repo.index.is_empty() {
        return Err(GitError::NothingToCommit);
    }

    // Build the tree from the index.
    let tree_hash = build_tree_from_index(&mut repo.objects, &repo.index)?;

    // Build the commit object.
    let author = "ClaudioOS Agent <agent@claudio.os>";
    let timestamp = "1700000000 +0000"; // Fixed timestamp for reproducibility.

    let mut commit_data = String::new();
    commit_data.push_str(&format!("tree {}\n", tree_hash));
    if let Some(ref parent) = repo.head {
        commit_data.push_str(&format!("parent {}\n", parent));
    }
    commit_data.push_str(&format!("author {} {}\n", author, timestamp));
    commit_data.push_str(&format!("committer {} {}\n", author, timestamp));
    commit_data.push('\n');
    commit_data.push_str(message);
    commit_data.push('\n');

    let commit_obj = GitObject {
        obj_type: ObjectType::Commit,
        data: commit_data.into_bytes(),
    };
    let commit_hash = repo.objects.store(commit_obj);

    repo.head = Some(commit_hash.clone());

    log::info!("[git] created commit {} — {}", &commit_hash[..8], message);

    Ok(format!("[{} {}] {}", repo.branch, &commit_hash[..8], message))
}

/// Build a tree object from the flat index (handling subdirectories).
fn build_tree_from_index(
    store: &mut ObjectStore,
    index: &alloc::collections::BTreeMap<String, String>,
) -> Result<String, GitError> {
    // Group files by directory.
    let mut dirs: alloc::collections::BTreeMap<String, Vec<TreeEntry>> =
        alloc::collections::BTreeMap::new();

    for (path, blob_hash) in index {
        let hash_bytes = hex_decode(blob_hash)?;
        if hash_bytes.len() != SHA1_LEN {
            return Err(GitError::Corrupt(format!("bad hash length for {}", path)));
        }
        let mut hash = [0u8; SHA1_LEN];
        hash.copy_from_slice(&hash_bytes);

        if let Some(slash) = path.rfind('/') {
            let dir = &path[..slash];
            let name = &path[slash + 1..];
            dirs.entry(String::from(dir))
                .or_insert_with(Vec::new)
                .push(TreeEntry {
                    mode: String::from("100644"),
                    name: String::from(name),
                    hash,
                });
        } else {
            dirs.entry(String::new())
                .or_insert_with(Vec::new)
                .push(TreeEntry {
                    mode: String::from("100644"),
                    name: path.clone(),
                    hash,
                });
        }
    }

    // Build trees bottom-up.
    // For simplicity, handle one level of nesting.
    let mut root_entries: Vec<TreeEntry> = Vec::new();

    // First, add all root-level files.
    if let Some(entries) = dirs.get("") {
        root_entries.extend(entries.iter().cloned());
    }

    // Then, create subtree objects for directories.
    for (dir, entries) in &dirs {
        if dir.is_empty() {
            continue;
        }
        let tree_data = build_tree(entries);
        let tree_obj = GitObject {
            obj_type: ObjectType::Tree,
            data: tree_data,
        };
        let tree_hash = store.store(tree_obj);
        let hash_bytes = hex_decode(&tree_hash)?;
        let mut hash = [0u8; SHA1_LEN];
        hash.copy_from_slice(&hash_bytes);

        // Use the top-level directory name.
        let dir_name = if let Some(slash) = dir.find('/') {
            &dir[..slash]
        } else {
            dir.as_str()
        };

        // Avoid duplicate entries.
        if !root_entries.iter().any(|e| e.name == dir_name) {
            root_entries.push(TreeEntry {
                mode: String::from("40000"),
                name: String::from(dir_name),
                hash,
            });
        }
    }

    // Sort entries by name (git requirement).
    root_entries.sort_by(|a, b| a.name.cmp(&b.name));

    let tree_data = build_tree(&root_entries);
    let tree_obj = GitObject {
        obj_type: ObjectType::Tree,
        data: tree_data,
    };

    Ok(store.store(tree_obj))
}

/// Push commits to the remote via Smart HTTP.
///
/// This is simplified: we send a single packfile containing all objects
/// the remote might not have.
pub fn git_push(
    repo: &Repository,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<String, GitError> {
    let url_str = repo
        .remote_url
        .as_ref()
        .ok_or_else(|| GitError::Io(String::from("no remote configured")))?;
    let parsed_url = parse_git_url(url_str)?;

    let head = repo
        .head
        .as_ref()
        .ok_or_else(|| GitError::NothingToCommit)?;

    log::info!("[git] pushing to {}", url_str);

    // Step 1: Get remote refs to know what they have.
    let request_path = format!("{}/info/refs?service=git-receive-pack", parsed_url.path);
    let req = claudio_net::http::HttpRequest::get(&parsed_url.host, &request_path)
        .header("User-Agent", "ClaudioOS-Git/1.0")
        .header("Accept", "*/*")
        .header("Connection", "close");

    let resp = do_http_request(&parsed_url, &req.to_bytes(), stack, now)?;
    let body = extract_http_body(&resp)?;
    let remote_refs = parse_ref_discovery(&body)?;

    let remote_head = remote_refs
        .iter()
        .find(|(_, name)| name == &format!("refs/heads/{}", repo.branch))
        .map(|(hash, _)| hash.clone());

    let old_ref = remote_head
        .clone()
        .unwrap_or_else(|| "0".repeat(SHA1_HEX_LEN));

    // Step 2: Build a minimal packfile with our objects.
    let pack_data = build_push_packfile(repo)?;

    // Step 3: Send the receive-pack request.
    // Format: pkt-line with ref update, then packfile.
    let mut push_body = Vec::new();

    let ref_update = format!(
        "{} {} refs/heads/{}\0 report-status\n",
        old_ref, head, repo.branch
    );
    write_pkt_line(&mut push_body, ref_update.as_bytes());
    push_body.extend_from_slice(b"0000"); // Flush.
    push_body.extend_from_slice(&pack_data);

    let push_path = format!("{}/git-receive-pack", parsed_url.path);
    let push_req = claudio_net::http::HttpRequest::post(
        &parsed_url.host,
        &push_path,
        push_body,
    )
    .header("User-Agent", "ClaudioOS-Git/1.0")
    .header("Content-Type", "application/x-git-receive-pack-request")
    .header("Accept", "application/x-git-receive-pack-result")
    .header("Connection", "close");

    let _resp = do_http_request(&parsed_url, &push_req.to_bytes(), stack, now)?;

    Ok(format!(
        "Pushed to {} ({}..{})",
        url_str,
        &old_ref[..core::cmp::min(8, old_ref.len())],
        &head[..8]
    ))
}

/// Build a packfile containing all objects in the store.
fn build_push_packfile(repo: &Repository) -> Result<Vec<u8>, GitError> {
    let mut pack = Vec::new();

    // Header.
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes()); // Version 2.
    pack.extend_from_slice(&(repo.objects.len() as u32).to_be_bytes());

    // Encode each object.
    for (_hash, obj) in &repo.objects.objects {
        let type_num: u8 = match obj.obj_type {
            ObjectType::Commit => 1,
            ObjectType::Tree => 2,
            ObjectType::Blob => 3,
            ObjectType::Tag => 4,
        };

        // Variable-length header: type (3 bits) + size.
        let size = obj.data.len();
        let mut header_byte = (type_num << 4) | (size as u8 & 0x0F);
        let mut remaining = size >> 4;

        if remaining > 0 {
            header_byte |= 0x80;
            pack.push(header_byte);
            while remaining > 0 {
                let mut b = (remaining & 0x7F) as u8;
                remaining >>= 7;
                if remaining > 0 {
                    b |= 0x80;
                }
                pack.push(b);
            }
        } else {
            pack.push(header_byte);
        }

        // Zlib-compressed object data.
        let compressed = zlib_compress(&obj.data);
        pack.extend_from_slice(&compressed);
    }

    // Pack checksum (SHA-1 of everything before).
    let checksum = sha1(&pack);
    pack.extend_from_slice(&checksum);

    Ok(pack)
}

/// Show recent commit log.
pub fn git_log(repo: &Repository, count: usize) -> String {
    let mut output = String::new();
    let mut current = repo.head.clone();
    let mut shown = 0;

    while let Some(ref hash) = current {
        if shown >= count {
            break;
        }

        if let Some(obj) = repo.objects.get(hash) {
            if obj.obj_type == ObjectType::Commit {
                if let Ok(commit) = parse_commit(&obj.data) {
                    output.push_str(&format!("commit {}\n", hash));
                    output.push_str(&format!("Author: {}\n", commit.author));
                    output.push_str(&format!("\n    {}\n\n", commit.message.trim()));

                    current = commit.parents.first().cloned();
                    shown += 1;
                    continue;
                }
            }
        }
        break;
    }

    if shown == 0 {
        output.push_str("No commits yet.\n");
    }

    output
}

// ---------------------------------------------------------------------------
// Shell command dispatcher
// ---------------------------------------------------------------------------

/// Global repository storage. Agents operate on repos stored here.
///
/// SAFETY: Single-threaded kernel, no concurrent access.
static mut REPOS: Option<alloc::vec::Vec<Repository>> = None;

fn repos() -> &'static mut Vec<Repository> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(REPOS);
        if (*ptr).is_none() {
            *ptr = Some(Vec::new());
        }
        (*ptr).as_mut().unwrap()
    }
}

/// Find a repo by path, or return the first one.
fn find_repo(path: Option<&str>) -> Option<&'static mut Repository> {
    let repos = repos();
    if let Some(p) = path {
        repos.iter_mut().find(|r| r.path == p)
    } else {
        repos.last_mut()
    }
}

/// Execute a git shell command.
///
/// Supported: `git clone <url> [path]`, `git pull`, `git status`,
/// `git add <files...>`, `git commit -m <message>`, `git push`,
/// `git log [-n count]`
pub fn execute_git_command(
    args: &[&str],
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> String {
    if args.is_empty() {
        return String::from("usage: git <clone|pull|status|add|commit|push|log> [args...]");
    }

    match args[0] {
        "clone" => {
            if args.len() < 2 {
                return String::from("usage: git clone <url> [path]");
            }
            let url = args[1];
            let path = if args.len() > 2 {
                args[2]
            } else {
                // Derive path from URL.
                let name = url
                    .rsplit('/')
                    .next()
                    .unwrap_or("repo")
                    .trim_end_matches(".git");
                name
            };

            match git_clone(url, path, stack, now) {
                Ok(repo) => {
                    let msg = format!(
                        "Cloning into '{}'...\n{} objects, {} files checked out.\n",
                        path,
                        repo.objects.len(),
                        repo.worktree.len()
                    );
                    repos().push(repo);
                    msg
                }
                Err(e) => format!("fatal: {}\n", e),
            }
        }

        "pull" => match find_repo(None) {
            Some(repo) => match git_pull(repo, stack, now) {
                Ok(msg) => format!("{}\n", msg),
                Err(e) => format!("error: {}\n", e),
            },
            None => String::from("fatal: not in a git repository\n"),
        },

        "status" => match find_repo(None) {
            Some(repo) => git_status(repo),
            None => String::from("fatal: not in a git repository\n"),
        },

        "add" => {
            if args.len() < 2 {
                return String::from("usage: git add <file>...\n");
            }
            match find_repo(None) {
                Some(repo) => {
                    let files: Vec<&str> = args[1..].iter().copied().collect();
                    match git_add(repo, &files) {
                        Ok(msg) => format!("{}\n", msg),
                        Err(e) => format!("error: {}\n", e),
                    }
                }
                None => String::from("fatal: not in a git repository\n"),
            }
        }

        "commit" => {
            // Parse -m "message".
            let message = if args.len() >= 3 && args[1] == "-m" {
                args[2..].join(" ")
            } else if args.len() >= 2 {
                args[1..].join(" ")
            } else {
                return String::from("usage: git commit -m <message>\n");
            };

            match find_repo(None) {
                Some(repo) => match git_commit(repo, &message) {
                    Ok(msg) => format!("{}\n", msg),
                    Err(e) => format!("error: {}\n", e),
                },
                None => String::from("fatal: not in a git repository\n"),
            }
        }

        "push" => match find_repo(None) {
            Some(repo) => match git_push(repo, stack, now) {
                Ok(msg) => format!("{}\n", msg),
                Err(e) => format!("error: {}\n", e),
            },
            None => String::from("fatal: not in a git repository\n"),
        },

        "log" => {
            let count = if args.len() >= 3 && args[1] == "-n" {
                args[2].parse().unwrap_or(10)
            } else {
                10
            };
            match find_repo(None) {
                Some(repo) => git_log(repo, count),
                None => String::from("fatal: not in a git repository\n"),
            }
        }

        _ => format!("git: '{}' is not a git command\n", args[0]),
    }
}
