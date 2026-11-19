//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};

use crate::mvcc::{CommitSequence, DatabaseId};

use super::{BackupError, BackupResult};

macro_rules! identity {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            pub fn new() -> BackupResult<Self> {
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes).map_err(|_| BackupError::Random)?;
                Self::from_bytes(bytes)
            }

            pub fn from_bytes(bytes: [u8; 16]) -> BackupResult<Self> {
                if bytes == [0; 16] {
                    return Err(BackupError::Invalid("zero identity"));
                }
                Ok(Self(bytes))
            }

            pub const fn as_bytes(self) -> [u8; 16] {
                self.0
            }
        }
    };
}

identity!(RepositoryId, "Identity of one immutable object repository.");
identity!(
    BackupId,
    "Identity of one attempted snapshot, preserved through publication resolution."
);
identity!(
    RestoreId,
    "Identity of one durable restoration into a new target directory."
);
identity!(
    SnapshotId,
    "Provider-owned identity of jointly pinned committed data and autonomous metadata."
);
identity!(
    NamespaceId,
    "Stable data namespace, independent of transaction-history identity."
);
identity!(
    ObjectId,
    "Identity of one immutable repository object, never a filesystem path."
);
identity!(
    ProtectionId,
    "Identity of one repository encryption-key domain; it contains no key material."
);

/// SHA-256 over an explicitly specified domain and encoded bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageDigest([u8; 32]);

impl ImageDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Unit content digest. The inventory separately binds its format, address, kind and length.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// Image mappings are distinct even when their physical database library is shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ImageProvider {
    Memory = 1,
    SQLiteNative = 2,
    SQLiteKV = 3,
    Redb = 4,
}

impl ImageProvider {
    pub(super) fn decode(value: u8) -> BackupResult<Self> {
        match value {
            1 => Ok(Self::Memory),
            2 => Ok(Self::SQLiteNative),
            3 => Ok(Self::SQLiteKV),
            4 => Ok(Self::Redb),
            _ => Err(BackupError::Unsupported("image provider")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFormat {
    provider: ImageProvider,
    version: u32,
}

impl ImageFormat {
    /// The provider owns image-version compatibility. The envelope never interprets its units.
    pub fn new(provider: ImageProvider, version: u32) -> BackupResult<Self> {
        if version == 0 {
            return Err(BackupError::Invalid("zero image version"));
        }
        Ok(Self { provider, version })
    }

    pub const fn provider(self) -> ImageProvider {
        self.provider
    }

    pub const fn version(self) -> u32 {
        self.version
    }

    pub(super) fn bytes(self) -> [u8; 5] {
        let mut bytes = [0; 5];
        bytes[0] = self.provider as u8;
        bytes[1..].copy_from_slice(&self.version.to_le_bytes());
        bytes
    }
}

/// This descriptor is meaningful only while its provider retains the matching joint snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCut {
    pub source: DatabaseId,
    pub snapshot: SnapshotId,
    pub format: ImageFormat,
    pub sequence: Option<CommitSequence>,
}

/// Providers advertise only their implemented image paths. A common record API is not sufficient.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupCapabilities {
    pub export: bool,
    pub import: bool,
    pub snapshot_fingerprints: bool,
}

impl BackupCapabilities {
    pub const UNSUPPORTED: Self = Self {
        export: false,
        import: false,
        snapshot_fingerprints: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupLimits {
    max_key_bytes: u32,
    max_frame_bytes: u32,
}

impl BackupLimits {
    /// Encoded keys and decoded frames have independent limits. Heap buffers also use the job's shared memory budget.
    pub fn new(max_key_bytes: u32, max_frame_bytes: u32) -> BackupResult<Self> {
        if max_key_bytes == 0 || max_frame_bytes == 0 {
            return Err(BackupError::Invalid("zero codec limit"));
        }
        Ok(Self {
            max_key_bytes,
            max_frame_bytes,
        })
    }

    pub const fn max_key_bytes(self) -> u32 {
        self.max_key_bytes
    }

    pub const fn max_frame_bytes(self) -> u32 {
        self.max_frame_bytes
    }

    pub(super) fn check_key(self, size: usize) -> BackupResult<()> {
        check_limit("unit address", size as u64, u64::from(self.max_key_bytes))
    }

    pub(super) fn check_frame(self, size: usize) -> BackupResult<()> {
        check_limit(
            "decoded frame",
            size as u64,
            u64::from(self.max_frame_bytes),
        )
    }
}

fn check_limit(resource: &'static str, required: u64, limit: u64) -> BackupResult<()> {
    if required > limit {
        Err(BackupError::Limit {
            resource,
            required,
            limit,
        })
    } else {
        Ok(())
    }
}

/// Ordered unit identity. Keys are opaque owner bytes, never repository or restore paths.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnitAddress<'a> {
    pub namespace: NamespaceId,
    pub generation: u64,
    pub family: u16,
    pub key: &'a [u8],
}

impl std::fmt::Debug for UnitAddress<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnitAddress")
            .field("namespace", &self.namespace)
            .field("generation", &self.generation)
            .field("family", &self.family)
            .field("key_bytes", &self.key.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum UnitKind {
    Metadata = 1,
    Record = 2,
    Tombstone = 3,
    Artifact = 4,
    Resource = 5,
}

impl UnitKind {
    pub(super) fn decode(value: u8) -> BackupResult<Self> {
        match value {
            1 => Ok(Self::Metadata),
            2 => Ok(Self::Record),
            3 => Ok(Self::Tombstone),
            4 => Ok(Self::Artifact),
            5 => Ok(Self::Resource),
            _ => Err(BackupError::Unsupported("unit kind")),
        }
    }
}

/// One contiguous sequence of frames inside an immutable object. A large unit spans frames without an in-memory reference vector. Verification must also check the actual object's length/digest, each frame and global reference overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectSpan {
    object: ObjectId,
    digest: ImageDigest,
    object_bytes: u64,
    offset: u64,
    encoded_bytes: u64,
    frames: u64,
}

impl ObjectSpan {
    pub const ENCODED_BYTES: usize = 80;

    pub fn new(
        object: ObjectId,
        digest: ImageDigest,
        object_bytes: u64,
        offset: u64,
        encoded_bytes: u64,
        frames: u64,
    ) -> BackupResult<Self> {
        if frames == 0
            || encoded_bytes == 0
            || offset
                .checked_add(encoded_bytes)
                .is_none_or(|end| end > object_bytes)
        {
            return Err(BackupError::Invalid("object span bounds"));
        }
        // Even an empty encrypted frame occupies its header and authentication tag.
        if frames > encoded_bytes / super::frame::MIN_FRAME_BYTES as u64 {
            return Err(BackupError::Invalid("object frame count"));
        }
        Ok(Self {
            object,
            digest,
            object_bytes,
            offset,
            encoded_bytes,
            frames,
        })
    }

    pub const fn object(self) -> ObjectId {
        self.object
    }
    pub const fn digest(self) -> ImageDigest {
        self.digest
    }
    pub const fn object_bytes(self) -> u64 {
        self.object_bytes
    }
    pub const fn offset(self) -> u64 {
        self.offset
    }
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }
    pub const fn frames(self) -> u64 {
        self.frames
    }

    pub fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        bytes[..16].copy_from_slice(&self.object.as_bytes());
        bytes[16..48].copy_from_slice(&self.digest.as_bytes());
        for (field, value) in bytes[48..].chunks_exact_mut(8).zip([
            self.object_bytes,
            self.offset,
            self.encoded_bytes,
            self.frames,
        ]) {
            field.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> BackupResult<Self> {
        if bytes.len() != Self::ENCODED_BYTES {
            return Err(BackupError::Invalid("object span length"));
        }
        Self::new(
            ObjectId::from_bytes(bytes[..16].try_into().unwrap())?,
            ImageDigest::from_bytes(bytes[16..48].try_into().unwrap()),
            u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
            u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
            u64::from_le_bytes(bytes[64..72].try_into().unwrap()),
            u64::from_le_bytes(bytes[72..80].try_into().unwrap()),
        )
    }
}
