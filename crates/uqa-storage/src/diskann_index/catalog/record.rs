//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The physical catalog key and the exact revision that authorized an index binding.

use crate::{
    key_value::{KeyValueRead, KeyValueReadRevision},
    read_control::StorageReadControl,
    KeyValueBatch, RelationIdentity, StorageBackendError, StorageBackendResult,
};
use uqa_core::memory::BudgetedVec;

pub struct DiskANNCatalogRecord {
    key: BudgetedVec<u8>,
    revision: KeyValueReadRevision,
}

impl DiskANNCatalogRecord {
    pub fn capture(
        read: &dyn KeyValueRead,
        key: BudgetedVec<u8>,
        missing: impl FnOnce() -> StorageBackendError,
    ) -> StorageBackendResult<Self> {
        let revision = read.record_revision(&key)?.ok_or_else(missing)?;
        Ok(Self { key, revision })
    }
    pub fn key(&self) -> &[u8] {
        &self.key
    }
    pub fn revision(&self) -> &KeyValueReadRevision {
        &self.revision
    }
}

/// Validate the actual record revisions and keep their unchanged requirements in the publication transaction.
pub fn require_current_records<'a>(
    records: impl IntoIterator<Item = &'a DiskANNCatalogRecord>,
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    control: &StorageReadControl,
    changed: impl FnOnce() -> StorageBackendError,
) -> StorageBackendResult<()> {
    for record in records {
        control.check()?;
        if read.record_revision(record.key())?.as_ref() != Some(record.revision()) {
            return Err(changed());
        }
        batch.require_unchanged(record.key())?;
    }
    control.check()
}

pub fn bound_relation(
    table: &str,
    index: &RelationIdentity,
    mismatch: impl FnOnce() -> StorageBackendError,
) -> StorageBackendResult<RelationIdentity> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
    if relation.qualified_name() != table || relation.schema != index.schema {
        return Err(mismatch());
    }
    Ok(relation)
}
