//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One private change, held in the memory tier or in a spilled run.

use std::sync::Arc;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::{
    CommitSequence, PreparedRecordWrite, PrivateRecordRevision, RecordMetadata, VersionResult,
};
use crate::read_control::StorageReadControl;

use super::super::run::{RunEntry, SpilledRun};
use super::super::Change;
use crate::mvcc::key::RecordKey;

/// A private change found in the memory tier, borrowed from its root, or in a spilled run, whose value stays in the run until a read asks for it.
pub(in crate::mvcc::overlay) enum TieredChange<'a> {
    Resident(&'a Change),
    Spilled {
        entry: RunEntry,
        run: Arc<SpilledRun>,
    },
}

impl TieredChange<'_> {
    pub(in crate::mvcc::overlay) fn key(&self) -> &[u8] {
        match self {
            Self::Resident(change) => change.write.key(),
            Self::Spilled { entry, .. } => entry.key.bytes(),
        }
    }

    pub(in crate::mvcc::overlay) fn shared_key(&self) -> RecordKey {
        match self {
            Self::Resident(change) => change.write.shared_key(),
            Self::Spilled { entry, .. } => entry.key.clone(),
        }
    }

    pub(in crate::mvcc::overlay) fn expected(&self) -> Option<CommitSequence> {
        match self {
            Self::Resident(change) => change.write.expected(),
            Self::Spilled { entry, .. } => entry.expected,
        }
    }

    pub(in crate::mvcc::overlay) fn metadata(&self) -> RecordMetadata {
        RecordMetadata {
            revision: self.expected(),
            live: match self {
                Self::Resident(change) => change.write.value().is_some(),
                Self::Spilled { entry, .. } => entry.value.is_some(),
            },
        }
    }

    pub(in crate::mvcc::overlay) fn kind(&self) -> RecordWriteKind {
        match self {
            Self::Resident(change) => change.write.kind(),
            Self::Spilled { entry, .. } => entry.kind,
        }
    }

    pub(in crate::mvcc::overlay) fn identity(&self) -> PrivateRecordRevision {
        match self {
            Self::Resident(change) => change.identity,
            Self::Spilled { entry, .. } => entry.identity,
        }
    }

    /// The change as a write, loading a spilled value under `control`.
    pub(in crate::mvcc::overlay) fn write(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<PreparedRecordWrite> {
        match self {
            Self::Resident(change) => Ok(change.write.clone()),
            Self::Spilled { entry, run } => {
                let value = entry
                    .value
                    .map(|location| run.load_value(location, control))
                    .transpose()?;
                Ok(
                    PreparedRecordWrite::from_shared(entry.key.clone(), entry.expected, value)
                        .with_kind(entry.kind),
                )
            }
        }
    }
}
