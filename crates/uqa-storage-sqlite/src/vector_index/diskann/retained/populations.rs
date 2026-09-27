//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed native headers expose populations; writable lifecycle owners alone initialize complete origin evidence.

use super::super::SQLiteDiskANNCanonical;
use super::{invalid, RetainedSQLiteDiskANNCanonical};
use crate::mvcc::native::{
    decode_record, populations, variable_fields_limit, NativeRecordOwner, NativeSnapshot,
};
use rusqlite::types::ValueRef;
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    diskann_index::{
        format::{DiskANNGeneration, DiskANNManifest, DiskANNOriginLayout},
        pages::DiskANNPageSource,
        DiskANNCanonicalCounts, DiskANNPopulationState,
    },
    key_value::{publication, KeyValueDiskANNSource},
    mvcc::VersionError,
    read_control::StorageReadControl,
    KeyValueBatch, StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    /// Reuse the actual complete-origin/ordinal validator on a fixed MVCC reconciliation view, including uncataloged canonical owners.
    pub(crate) fn from_population(
        snapshot: NativeSnapshot,
        owner: NativeRecordOwner,
        table: BudgetedVec<u8>,
        field: BudgetedVec<u8>,
        dimensions: u32,
    ) -> StorageBackendResult<Self> {
        snapshot.control.check()?;
        let memory = snapshot
            .control
            .memory()
            .reserve(size_of::<Self>() + size_of::<NativeSnapshot>())?;
        let control = snapshot.control.clone();
        Ok(Self {
            snapshot: Arc::new(snapshot),
            owner: Some(owner),
            table,
            field,
            dimensions,
            control,
            binding: None,
            _memory: memory,
        })
    }

    pub(in crate::vector_index::diskann) fn read_population(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
        self.check(control)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        let key = populations::header_key(owner, &self.field, generation, control)
            .map_err(VersionError::into_storage_error)?;
        let maximum = variable_fields_limit(&[
            self.table.len(),
            self.field.len(),
            40,
            DiskANNPopulationState::ENCODED_BYTES,
        ])
        .map_err(VersionError::into_storage_error)?;
        let mut result = None;
        self.snapshot
            .view
            .visit_value_bounded(&key, maximum, control, &mut |record| {
                self.check(control)?;
                let Some(value) = record.and_then(|record| record.value) else {
                    return Ok(());
                };
                let (_, row) = decode_record(&key, value, control)?;
                if row[0] != ValueRef::Text(&self.table) {
                    return Err(VersionError::InvalidEncoding(
                        "native population table scope mismatch",
                    ));
                }
                let bytes = row[3].as_blob().map_err(|_| {
                    VersionError::InvalidEncoding("native population is not binary")
                })?;
                result = Some(
                    DiskANNPopulationState::decode(bytes, generation, self.dimensions)?.counts(),
                );
                Ok(())
            })
            .map_err(VersionError::into_storage_error)?;
        self.check(control)?;
        Ok(result)
    }

    pub(in crate::vector_index::diskann) fn publish_population(
        &self,
        sealed: &KeyValueDiskANNSource,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.check(control)?;
        let maximum =
            DiskANNManifest::MAX_ENCODED_BYTES.max(DiskANNOriginLayout::MAX_ENCODED_BYTES);
        let origins = sealed.origin_reader(maximum, control)?;
        let state = DiskANNPopulationState::from_counts(
            origins.manifest().input().generation,
            self.dimensions,
            DiskANNCanonicalCounts::default(),
        )?;
        let template = populations::header_record(
            self.owner
                .ok_or_else(|| invalid("population publication has no canonical owner"))?,
            &self.table,
            &self.field,
            state,
            control,
        )
        .map_err(VersionError::into_storage_error)?;
        batch.publish_diskann_population(template.key(), template.row(), origins)
    }

    pub(in crate::vector_index::diskann) fn retire_population(
        &self,
        generation: DiskANNGeneration,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.check(control)?;
        let key = populations::header_key(
            self.owner
                .ok_or_else(|| invalid("population retirement has no canonical owner"))?,
            &self.field,
            generation,
            control,
        )
        .map_err(VersionError::into_storage_error)?;
        batch.retire_diskann_population(&key)
    }
}

impl SQLiteDiskANNCanonical {
    pub(in crate::vector_index::diskann) fn initialize_populations(
        &self,
        index: &uqa_storage::RelationIdentity,
        resolver: &dyn uqa_storage::diskann_index::catalog::DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let source = self.retain_for_index(index, control)?;
        let selected = source
            .selected_source(resolver, control)?
            .ok_or_else(|| invalid("population initialization has no selected generation"))?;
        let generation = selected.generation();
        if source.read_population(generation, control)?.is_some() {
            return Ok(());
        }
        let scope = source.index_scope(resolver, control)?;
        self.index
            .conn
            .with_native_write(|snapshot, batch| {
                let read = snapshot.record_read();
                source.require_current_index(&read, batch, control)?;
                {
                    let mapped = crate::diskann::map_read(&read, snapshot.database)?;
                    let mut mapped_batch =
                        crate::diskann::map_batch(batch, snapshot.database, &snapshot.control)?;
                    publication::require_selected_generation(
                        &scope,
                        &mapped,
                        &mut mapped_batch,
                        generation,
                        control,
                    )?;
                }
                source.publish_population(&selected, batch, control)?;
                Ok(())
            })?
            .ok_or_else(|| invalid("population initialization requires a native session"))
    }
}
