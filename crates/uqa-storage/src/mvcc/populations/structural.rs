//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The canonical writes of a batch, which own the population headers they replace.

use crate::mvcc::commit::{PreparedLookup, RecordWriteKind};
use crate::mvcc::{PreparedRecordCommit, PreparedRecordWrite, VersionResult};
use crate::read_control::StorageReadControl;

/// Finds the canonical write of a key in a batch being prepared, whether the batch is in memory or spilled.
pub(super) struct StructuralRecords<'a>(PreparedLookup<'a>);

impl<'a> StructuralRecords<'a> {
    pub(super) fn new(
        prepared: &'a PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        PreparedLookup::new(prepared, control).map(Self)
    }

    /// The canonical write of `key`, if the batch has one.
    pub(super) fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        Ok(self
            .0
            .get(key, control)?
            .filter(|write| write.kind() == RecordWriteKind::Canonical))
    }
}
