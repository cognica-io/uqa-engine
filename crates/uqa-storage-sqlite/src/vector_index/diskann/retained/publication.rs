//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog guards and generation selection share the caller's native record mutation.

use super::{invalid, Family, Identity, RetainedSQLiteDiskANNCanonical};
use crate::mvcc::native::NativeSnapshot;
use rusqlite::types::ValueRef;
use uqa_storage::{
    diskann_index::{build::DiskANNCanonicalCoverage, catalog::DiskANNIndexResolver},
    key_value::{publication, KeyValueRead},
    mvcc::VersionError,
    read_control::StorageReadControl,
    KeyValueBatch, StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    pub(crate) fn retire_generation(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        current: &NativeSnapshot,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::diskann_index::format::DiskANNGeneration> {
        let captured_read = self.snapshot.record_read();
        let current_read = current.record_read();
        self.require_current_index(&current_read, batch, control)?;
        let scope = self.index_scope(resolver, control)?;
        let captured = crate::diskann::map_read(&captured_read, self.snapshot.database)?;
        let current_records = crate::diskann::map_read(&current_read, current.database)?;
        let mut batch = crate::diskann::map_batch(batch, current.database, &current.control)?;
        publication::retire_captured_generation(
            &scope,
            &captured,
            &current_records,
            &mut batch,
            control,
        )
    }

    pub(crate) fn publish_generation(
        coverage: &DiskANNCanonicalCoverage<Self>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &uqa_storage::key_value::KeyValueDiskANNSource,
        current: &NativeSnapshot,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        coverage.check_control(control)?;
        let source = coverage.source();
        let captured_read = source.snapshot.record_read();
        let current_read = current.record_read();
        source.require_current_index(&current_read, batch, control)?;
        let owner = source
            .owner
            .ok_or_else(|| invalid("missing canonical owner"))?;
        let prefix = |family| {
            Identity::new(family, owner)
                .and_then(|identity| {
                    identity.encode_prefix(&[ValueRef::Text(&source.field)], control)
                })
                .map_err(VersionError::into_storage_error)
        };
        let vectors = prefix(Family::Vectors)?;
        let origins = prefix(Family::VectorOrigins)?;
        let prefixes = [&*vectors, &*origins];
        let original = captured_read.revision(&prefixes)?;
        if original.has_private_changes()
            && !original.same_private_changes(&current_read.revision(&prefixes)?)
        {
            return Err(invalid(
                "captured private canonical input changed before publication",
            ));
        }
        let scope = source.index_scope(resolver, control)?;
        let captured = crate::diskann::map_read(&captured_read, source.snapshot.database)?;
        let current_records = crate::diskann::map_read(&current_read, current.database)?;
        let mut batch = crate::diskann::map_batch(batch, current.database, &current.control)?;
        publication::publish_captured_generation(
            coverage,
            &scope,
            source
                .index_parameters()
                .ok_or_else(|| invalid("missing index parameters"))?,
            publication::DiskANNPublicationViews {
                captured: &captured,
                current: &current_records,
                batch: &mut batch,
                sealed,
            },
            control,
        )
    }
}
