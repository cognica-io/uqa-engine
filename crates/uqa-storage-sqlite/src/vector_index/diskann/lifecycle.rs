//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native creation and rebuilding use the common builder and the original catalog transaction.

use super::{invalid, RetainedSQLiteDiskANNCanonical, SQLiteDiskANNCanonical};
use crate::{mvcc::native::NativeRecordFamily as Family, vector_index::native::records};
use rusqlite::types::ValueRef;
use uqa_storage::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        catalog::DiskANNIndexResolver,
        format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
        DiskANNIndexOptions,
    },
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendError, StorageBackendResult, StorageSavepointId,
};

impl SQLiteDiskANNCanonical {
    /// Adopt the complete existing raw field and publish its first generation inside the caller's active catalog transaction. Its private table/index definitions must already exist.
    pub fn create_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.structural_change(|| {
            if self
                .retain_for_index(index, control)?
                .selected_source(resolver, control)?
                .is_some()
            {
                return Err(invalid("index already has a published generation"));
            }
            self.adopt(index, control)?;
            self.rebuild_index(index, resolver, options, temporary, control)
        })
    }

    /// Retain one canonical/catalog view, construct through the common bounded builder, and install its original expected head in this native transaction.
    pub fn rebuild_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.rebuild_source(
            self.retain_for_index(index, control)?,
            resolver,
            options,
            temporary,
            control,
        )
    }

    pub(super) fn rebuild_source(
        &self,
        source: RetainedSQLiteDiskANNCanonical,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.require_transaction()?;
        if source.index_parameters() != Some(options.parameters) {
            return Err(invalid("construction parameters differ from the catalog"));
        }
        let scope = source.index_scope(resolver, control)?;
        let repository = self.index.conn.diskann_generations(control)?;
        repository.initialize(control)?;
        let mut stage = repository.allocate_bound_stage(&scope, control)?;
        let coverage = stage.build(source, options, temporary, control)?;
        let sealed = repository.open_source(stage.generation(), control)?;
        let _prepared = uqa_storage::diskann_index::DiskANNQuery::open(
            coverage.source(),
            sealed,
            options.parameters,
            options.read,
            control,
        )?;
        self.index
            .conn
            .publish_diskann_generation(&coverage, resolver, control)
    }

    pub(super) fn clear_index(
        &self,
        source: &RetainedSQLiteDiskANNCanonical,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.structural_change(|| {
            self.index
                .conn
                .with_native_write(|snapshot, batch| {
                    source.require_current_index(&snapshot.record_read(), batch, control)?;
                    crate::vector_index::native::NativeVectorRead::new(snapshot, &self.index)?
                        .clear_family(batch, Family::Vectors)?;
                    control.check()?;
                    Ok(())
                })?
                .ok_or_else(|| invalid("clear requires a native record session"))?;
            self.rebuild_index(index, resolver, options, temporary, control)
        })
    }

    fn adopt(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index
            .conn
            .with_native_versioned_write(|origin, snapshot, batch| {
                let binding = crate::catalog::DiskANNCatalogBinding::capture(
                    snapshot,
                    &self.index.table,
                    &self.index.field,
                    self.index.dimensions,
                    index,
                    control,
                )?;
                binding.require_current(&snapshot.record_read(), batch, control)?;
                let version = DiskANNVectorVersion::new(origin.transaction(), origin.revision())?;
                let owner = snapshot.ensure_table_owner(&self.index.table, batch)?;
                let field = ValueRef::Text(self.index.field.as_bytes());
                let table = ValueRef::Text(self.index.table.as_bytes());
                snapshot.coordinate_vector_field(batch, owner, field, true)?;
                for family in [Family::VectorOrigins, Family::VectorChanges] {
                    snapshot.delete_prefix(batch, family, owner, &[field])?;
                }
                let stamp = |document: u64,
                             count,
                             batch: &mut dyn uqa_storage::KeyValueBatch|
                 -> crate::Result<()> {
                    let record =
                        DiskANNCanonicalOrigin::new(version, self.index.dimensions, count)?;
                    snapshot.put_row(
                        batch,
                        Family::VectorOrigins,
                        owner,
                        &[
                            table,
                            field,
                            ValueRef::Integer(document as i64),
                            ValueRef::Blob(&record.encode()),
                        ],
                    )?;
                    snapshot.put_row(
                        batch,
                        Family::VectorChanges,
                        owner,
                        &[
                            table,
                            field,
                            ValueRef::Blob(&DiskANNChangeIdentity::new(document, version).encode()),
                            ValueRef::Blob(&record.encode()),
                        ],
                    )?;
                    Ok(())
                };
                let mut current = None;
                let mut count = 0_u64;
                snapshot.visit_rows(Family::Vectors, Some(owner), &[field], |row| {
                    control.check()?;
                    let document = records::unsigned(records::integer(row[2])?)?;
                    let ordinal = records::ordinal(records::integer(row[3])?)?;
                    let vector = records::vector(row[4], control)?;
                    uqa_storage::vector_index::validate_vector_values_controlled(
                        self.index.dimensions,
                        &vector,
                        Some(control),
                    )?;
                    if current != Some(document) {
                        if let Some(previous) = current {
                            stamp(previous, count, batch)?;
                        }
                        current = Some(document);
                        count = 0;
                    }
                    if u64::from(ordinal) != count {
                        return Err(invalid("canonical ordinal sequence has a gap").into());
                    }
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| invalid("canonical count overflow"))?;
                    Ok(())
                })?;
                if let Some(document) = current {
                    stamp(document, count, batch)?;
                }
                control.check()?;
                Ok(())
            })?;
        Ok(())
    }

    fn structural_change(
        &self,
        operation: impl FnOnce() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.require_transaction()?;
        let connection = &self.index.conn;
        let savepoint = StorageSavepointId::allocate().backend_name();
        connection.savepoint(&savepoint)?;
        match operation() {
            Ok(()) => Ok(connection.release_savepoint(&savepoint)?),
            Err(error) => {
                connection.rollback_to_savepoint(&savepoint).and_then(|()| connection.release_savepoint(&savepoint))
                    .map_err(|rollback| StorageBackendError::Other(format!("DiskANN structural rollback failed: {rollback}; original error: {error}")))?;
                Err(error)
            }
        }
    }

    fn require_transaction(&self) -> StorageBackendResult<()> {
        if !self.index.conn.in_transaction() {
            return Err(invalid(
                "structural changes require the caller's active catalog transaction",
            ));
        }
        Ok(())
    }
}
