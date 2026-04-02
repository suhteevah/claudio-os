//! btrfs key structure and key type constants.
//!
//! Every item in a btrfs B-tree is addressed by a `BtrfsKey` which is a triple of
//! (objectid, type, offset). Keys are sorted lexicographically: first by objectid,
//! then by type, then by offset.
//!
//! The key type byte determines what kind of item the key refers to.
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#key-btrfs-disk-key>

use core::cmp::Ordering;
use core::fmt;

/// Size of a btrfs key on disk (17 bytes: u64 + u8 + u64).
pub const BTRFS_KEY_SIZE: usize = 17;

/// btrfs key type constants.
///
/// These identify the kind of item stored in the B-tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyType {
    /// Inode item (metadata: size, mode, timestamps, etc.).
    InodeItem = 1,
    /// Inode reference (hardlink name + parent directory objectid).
    InodeRef = 12,
    /// Inode extended reference (for names > 255 bytes or many hardlinks).
    InodeExtref = 13,
    /// Extended attribute item.
    XattrItem = 24,
    /// Orphan item (inode awaiting cleanup).
    OrphanItem = 48,
    /// Directory item (keyed by name hash).
    DirItem = 84,
    /// Directory index (keyed by sequence number for readdir ordering).
    DirIndex = 96,
    /// Extent data (file contents mapping).
    ExtentData = 108,
    /// Extent checksum item.
    ExtentCsum = 128,
    /// Root item (describes a subvolume/snapshot root).
    RootItem = 132,
    /// Root backref.
    RootBackref = 144,
    /// Root reference.
    RootRef = 156,
    /// Extent item (extent allocation metadata).
    ExtentItem = 168,
    /// Metadata item (thin metadata extent).
    MetadataItem = 169,
    /// Tree block reference (extent back-reference).
    TreeBlockRef = 176,
    /// Extent data reference.
    ExtentDataRef = 178,
    /// Shared block reference.
    SharedBlockRef = 182,
    /// Shared data reference.
    SharedDataRef = 184,
    /// Block group item.
    BlockGroupItem = 192,
    /// Free space info item.
    FreeSpaceInfo = 198,
    /// Free space extent.
    FreeSpaceExtent = 199,
    /// Free space bitmap.
    FreeSpaceBitmap = 200,
    /// Device extent item.
    DevExtent = 204,
    /// Device item (describes a physical device in the filesystem).
    DevItem = 216,
    /// Chunk item (logical to physical mapping).
    ChunkItem = 228,
    /// Temporary item (used during balance/relocation).
    TemporaryItem = 248,
    /// Unknown key type (catch-all).
    Unknown = 0,
}

impl KeyType {
    /// Convert a raw u8 key type byte to a `KeyType` enum variant.
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => KeyType::InodeItem,
            12 => KeyType::InodeRef,
            13 => KeyType::InodeExtref,
            24 => KeyType::XattrItem,
            48 => KeyType::OrphanItem,
            84 => KeyType::DirItem,
            96 => KeyType::DirIndex,
            108 => KeyType::ExtentData,
            128 => KeyType::ExtentCsum,
            132 => KeyType::RootItem,
            144 => KeyType::RootBackref,
            156 => KeyType::RootRef,
            168 => KeyType::ExtentItem,
            169 => KeyType::MetadataItem,
            176 => KeyType::TreeBlockRef,
            178 => KeyType::ExtentDataRef,
            182 => KeyType::SharedBlockRef,
            184 => KeyType::SharedDataRef,
            192 => KeyType::BlockGroupItem,
            198 => KeyType::FreeSpaceInfo,
            199 => KeyType::FreeSpaceExtent,
            200 => KeyType::FreeSpaceBitmap,
            204 => KeyType::DevExtent,
            216 => KeyType::DevItem,
            228 => KeyType::ChunkItem,
            248 => KeyType::TemporaryItem,
            _ => {
                log::warn!("[btrfs::key] unknown key type: {}", val);
                KeyType::Unknown
            }
        }
    }
}

/// Well-known objectid constants.
pub mod objectid {
    /// The root tree (tree of tree roots).
    pub const ROOT_TREE_OBJECTID: u64 = 1;
    /// The extent tree (block allocation).
    pub const EXTENT_TREE_OBJECTID: u64 = 2;
    /// The chunk tree (logical-to-physical mapping).
    pub const CHUNK_TREE_OBJECTID: u64 = 3;
    /// The device tree (per-device allocation info).
    pub const DEV_TREE_OBJECTID: u64 = 4;
    /// The filesystem tree (default subvolume).
    pub const FS_TREE_OBJECTID: u64 = 5;
    /// The root tree directory (for listing subvolumes).
    pub const ROOT_TREE_DIR_OBJECTID: u64 = 6;
    /// The checksum tree.
    pub const CSUM_TREE_OBJECTID: u64 = 7;
    /// The quota tree.
    pub const QUOTA_TREE_OBJECTID: u64 = 8;
    /// The UUID tree.
    pub const UUID_TREE_OBJECTID: u64 = 9;
    /// The free space tree.
    pub const FREE_SPACE_TREE_OBJECTID: u64 = 10;
    /// The block group tree.
    pub const BLOCK_GROUP_TREE_OBJECTID: u64 = 11;
    /// First free objectid for user data.
    pub const FIRST_FREE_OBJECTID: u64 = 256;
    /// Last free objectid.
    pub const LAST_FREE_OBJECTID: u64 = u64::MAX - 256;
    /// Device items objectid in the chunk tree.
    pub const DEV_ITEMS_OBJECTID: u64 = 1;
    /// First chunk tree objectid.
    pub const FIRST_CHUNK_TREE_OBJECTID: u64 = 256;
}

/// A btrfs B-tree key.
///
/// Keys are the primary addressing mechanism in btrfs. Every item in every tree
/// is identified by a unique key. Keys sort lexicographically by (objectid, type, offset).
#[derive(Clone, Copy, Eq)]
pub struct BtrfsKey {
    /// The object this key belongs to (e.g., inode number, tree id).
    pub objectid: u64,
    /// The type of item (see `KeyType`).
    pub key_type: u8,
    /// Type-specific offset (e.g., byte offset for extent data, hash for dir items).
    pub offset: u64,
}

impl BtrfsKey {
    /// Parse a key from a 17-byte on-disk buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_KEY_SIZE {
            log::error!("[btrfs::key] buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_KEY_SIZE);
            return None;
        }

        let key = BtrfsKey {
            objectid: read_u64(buf, 0),
            key_type: buf[8],
            offset: read_u64(buf, 9),
        };

        log::trace!("[btrfs::key] parsed: objectid={}, type={}({}), offset={}",
            key.objectid, key.key_type, key.type_name(), key.offset);

        Some(key)
    }

    /// Serialize this key to a 17-byte buffer.
    pub fn to_bytes(&self) -> [u8; BTRFS_KEY_SIZE] {
        let mut buf = [0u8; BTRFS_KEY_SIZE];
        buf[0..8].copy_from_slice(&self.objectid.to_le_bytes());
        buf[8] = self.key_type;
        buf[9..17].copy_from_slice(&self.offset.to_le_bytes());
        buf
    }

    /// Create a new key with the given fields.
    #[inline]
    pub fn new(objectid: u64, key_type: u8, offset: u64) -> Self {
        BtrfsKey { objectid, key_type, offset }
    }

    /// Get the key type as a `KeyType` enum variant.
    #[inline]
    pub fn key_type_enum(&self) -> KeyType {
        KeyType::from_u8(self.key_type)
    }

    /// Human-readable name for this key's type.
    pub fn type_name(&self) -> &'static str {
        match self.key_type_enum() {
            KeyType::InodeItem => "INODE_ITEM",
            KeyType::InodeRef => "INODE_REF",
            KeyType::InodeExtref => "INODE_EXTREF",
            KeyType::XattrItem => "XATTR_ITEM",
            KeyType::OrphanItem => "ORPHAN_ITEM",
            KeyType::DirItem => "DIR_ITEM",
            KeyType::DirIndex => "DIR_INDEX",
            KeyType::ExtentData => "EXTENT_DATA",
            KeyType::ExtentCsum => "EXTENT_CSUM",
            KeyType::RootItem => "ROOT_ITEM",
            KeyType::RootBackref => "ROOT_BACKREF",
            KeyType::RootRef => "ROOT_REF",
            KeyType::ExtentItem => "EXTENT_ITEM",
            KeyType::MetadataItem => "METADATA_ITEM",
            KeyType::TreeBlockRef => "TREE_BLOCK_REF",
            KeyType::ExtentDataRef => "EXTENT_DATA_REF",
            KeyType::SharedBlockRef => "SHARED_BLOCK_REF",
            KeyType::SharedDataRef => "SHARED_DATA_REF",
            KeyType::BlockGroupItem => "BLOCK_GROUP_ITEM",
            KeyType::FreeSpaceInfo => "FREE_SPACE_INFO",
            KeyType::FreeSpaceExtent => "FREE_SPACE_EXTENT",
            KeyType::FreeSpaceBitmap => "FREE_SPACE_BITMAP",
            KeyType::DevExtent => "DEV_EXTENT",
            KeyType::DevItem => "DEV_ITEM",
            KeyType::ChunkItem => "CHUNK_ITEM",
            KeyType::TemporaryItem => "TEMPORARY_ITEM",
            KeyType::Unknown => "UNKNOWN",
        }
    }

    /// Create a minimum key (used as search lower bound).
    #[inline]
    pub fn min() -> Self {
        BtrfsKey { objectid: 0, key_type: 0, offset: 0 }
    }

    /// Create a maximum key (used as search upper bound).
    #[inline]
    pub fn max() -> Self {
        BtrfsKey { objectid: u64::MAX, key_type: u8::MAX, offset: u64::MAX }
    }
}

impl PartialEq for BtrfsKey {
    fn eq(&self, other: &Self) -> bool {
        self.objectid == other.objectid
            && self.key_type == other.key_type
            && self.offset == other.offset
    }
}

impl PartialOrd for BtrfsKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BtrfsKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.objectid.cmp(&other.objectid)
            .then(self.key_type.cmp(&other.key_type))
            .then(self.offset.cmp(&other.offset))
    }
}

impl fmt::Debug for BtrfsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsKey")
            .field("objectid", &self.objectid)
            .field("type", &format_args!("{}({})", self.key_type, self.type_name()))
            .field("offset", &self.offset)
            .finish()
    }
}

impl fmt::Display for BtrfsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.objectid, self.type_name(), self.offset)
    }
}

// --- Little-endian byte helpers ---

#[inline]
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3],
        buf[offset + 4], buf[offset + 5], buf[offset + 6], buf[offset + 7],
    ])
}
