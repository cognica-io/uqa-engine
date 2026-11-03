//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mvcc::{CommitSequence, DatabaseId};

use super::{
    BackupCut, BackupError, BackupId, BackupResult, ImageFormat, ImageProvider, InventorySummary,
    ObjectSpan, RepositoryId, SnapshotId,
};

/// Fixed root descriptor inside an authenticated root frame. Decoding does not publish a backup or verify the referenced object graph. The complete inventory is authoritative; `parent` records provenance only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupRoot {
    repository: RepositoryId,
    backup: BackupId,
    parent: Option<BackupId>,
    cut: BackupCut,
    inventory: ObjectSpan,
    summary: InventorySummary,
}

impl BackupRoot {
    pub const ENCODED_BYTES: usize = 240;

    pub fn new(
        repository: RepositoryId,
        backup: BackupId,
        parent: Option<BackupId>,
        cut: BackupCut,
        inventory: ObjectSpan,
        summary: InventorySummary,
    ) -> BackupResult<Self> {
        if parent == Some(backup) {
            return Err(BackupError::Invalid("self-parent backup"));
        }
        if cut.source.as_bytes() == [0; 16] {
            return Err(BackupError::Invalid("zero source incarnation"));
        }
        if summary.units == 0 && summary.decoded_bytes != 0 {
            return Err(BackupError::Invalid("empty inventory byte count"));
        }
        Ok(Self {
            repository,
            backup,
            parent,
            cut,
            inventory,
            summary,
        })
    }

    pub const fn repository(self) -> RepositoryId {
        self.repository
    }
    pub const fn backup(self) -> BackupId {
        self.backup
    }
    pub const fn parent(self) -> Option<BackupId> {
        self.parent
    }
    pub const fn cut(self) -> BackupCut {
        self.cut
    }
    pub const fn inventory(self) -> ObjectSpan {
        self.inventory
    }
    pub const fn summary(self) -> InventorySummary {
        self.summary
    }

    /// Reject foreign repositories, restored source incarnations and changed provider image formats before comparing an incremental parent. Each snapshot identity and sequence may differ.
    pub fn validate_parent(self, parent: Self) -> BackupResult<()> {
        if self.parent != Some(parent.backup)
            || self.repository != parent.repository
            || self.cut.source != parent.cut.source
            || self.cut.format != parent.cut.format
        {
            return Err(BackupError::Invalid("incompatible incremental parent"));
        }
        Ok(())
    }

    pub fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        bytes[..8].copy_from_slice(b"UQAROOT1");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[16..32].copy_from_slice(&self.repository.as_bytes());
        bytes[32..48].copy_from_slice(&self.backup.as_bytes());
        if let Some(parent) = self.parent {
            bytes[48..64].copy_from_slice(&parent.as_bytes());
        }
        bytes[64..80].copy_from_slice(&self.cut.source.as_bytes());
        bytes[80..96].copy_from_slice(&self.cut.snapshot.as_bytes());
        bytes[96..101].copy_from_slice(&self.cut.format.bytes());
        if let Some(sequence) = self.cut.sequence {
            bytes[101] = 1;
            bytes[104..112].copy_from_slice(&sequence.as_u64().to_le_bytes());
        }
        bytes[112..192].copy_from_slice(&self.inventory.encode());
        bytes[192..].copy_from_slice(&self.summary.encode());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> BackupResult<Self> {
        if bytes.len() != Self::ENCODED_BYTES || &bytes[..8] != b"UQAROOT1" {
            return Err(BackupError::Invalid("root length or magic"));
        }
        if bytes[8..10] != 1_u16.to_le_bytes() {
            return Err(BackupError::Unsupported("root version"));
        }
        if bytes[10..16] != [0; 6] || bytes[102..104] != [0; 2] {
            return Err(BackupError::Invalid("root reserved fields"));
        }
        let sequence = u64::from_le_bytes(bytes[104..112].try_into().unwrap());
        let sequence = match (bytes[101], sequence) {
            (0, 0) => None,
            (1, sequence) => Some(CommitSequence::from_u64(sequence)),
            _ => return Err(BackupError::Invalid("root sequence tag")),
        };
        let parent = bytes[48..64].try_into().unwrap();
        let parent = if parent == [0; 16] {
            None
        } else {
            Some(BackupId::from_bytes(parent)?)
        };
        Self::new(
            RepositoryId::from_bytes(bytes[16..32].try_into().unwrap())?,
            BackupId::from_bytes(bytes[32..48].try_into().unwrap())?,
            parent,
            BackupCut {
                source: DatabaseId::from_bytes(bytes[64..80].try_into().unwrap()),
                snapshot: SnapshotId::from_bytes(bytes[80..96].try_into().unwrap())?,
                format: ImageFormat::new(
                    ImageProvider::decode(bytes[96])?,
                    u32::from_le_bytes(bytes[97..101].try_into().unwrap()),
                )?,
                sequence,
            },
            ObjectSpan::decode(&bytes[112..192])?,
            InventorySummary::decode(bytes[192..].try_into().unwrap())?,
        )
    }
}
