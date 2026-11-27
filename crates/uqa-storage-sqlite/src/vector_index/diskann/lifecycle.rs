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
        build::{DiskANNCanonicalCoverage, DiskANNTemporaryBudget},
        catalog::DiskANNIndexResolver,
        format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
        DiskANNAdoptedOrigins, DiskANNIndexOptions, DiskANNLifecycle,
    },
    key_value::{KeyValueDiskANNSource, KeyValueDiskANNStore},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

impl SQLiteDiskANNCanonical {
    /// Retire the selected generation in the caller's transaction before its SQL catalog row is removed. Preserve raw vectors for the replacement exact index and fence obsolete writers.
    pub fn retire_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        DiskANNLifecycle::retire_index(self, index, resolver, control)
    }
    /// Adopt the complete existing raw field and publish its first generation inside the caller's active catalog transaction. Its private table/index definitions must already exist.
    pub fn create_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        DiskANNLifecycle::create_index(self, index, resolver, options, temporary, control)
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
        DiskANNLifecycle::rebuild_index(self, index, resolver, options, temporary, control)
    }
}

impl DiskANNLifecycle for SQLiteDiskANNCanonical {
    type Read = RetainedSQLiteDiskANNCanonical;
    type SavepointError = crate::SQLiteError;
    fn in_transaction(&self) -> bool {
        self.index.conn.in_transaction()
    }
    fn savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.conn.savepoint(name)
    }
    fn release_savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.conn.release_savepoint(name)
    }
    fn rollback_to_savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.conn.rollback_to_savepoint(name)
    }
    fn retain_for_index(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self::Read> {
        Self::retain_for_index(self, index, control)
    }
    fn generation_store(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStore> {
        self.index.conn.diskann_generations(control)
    }
    fn publish(
        &self,
        coverage: &DiskANNCanonicalCoverage<Self::Read>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index
            .conn
            .publish_sealed_diskann_generation(coverage, resolver, sealed, control)
    }
    fn retire_generation(
        &self,
        source: &Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index
            .conn
            .with_native_write(|snapshot, batch| {
                source.retire_generation(resolver, snapshot, batch, control)?;
                let owner = snapshot.ensure_table_owner(&self.index.table, batch)?;
                snapshot.coordinate_vector_field(
                    batch,
                    owner,
                    ValueRef::Text(self.index.field.as_bytes()),
                    true,
                )?;
                control.check()?;
                Ok(())
            })?
            .ok_or_else(|| invalid("retirement requires a native record session"))
    }
    fn clear_vectors(
        &self,
        source: &Self::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index
            .conn
            .with_native_write(|snapshot, batch| {
                source.require_current_index(&snapshot.record_read(), batch, control)?;
                crate::vector_index::native::NativeVectorRead::new(snapshot, &self.index)?
                    .clear_family(batch, Family::Vectors)?;
                control.check()?;
                Ok(())
            })?
            .ok_or_else(|| invalid("clear requires a native record session"))
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
                let mut origins = DiskANNAdoptedOrigins::new(
                    version,
                    self.index.dimensions,
                    |document, record: DiskANNCanonicalOrigin| -> crate::Result<()> {
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
                                ValueRef::Blob(
                                    &DiskANNChangeIdentity::new(document, version).encode(),
                                ),
                                ValueRef::Blob(&record.encode()),
                            ],
                        )?;
                        Ok(())
                    },
                );
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
                    origins.push(document, ordinal)
                })?;
                origins.finish()?;
                control.check()?;
                Ok(())
            })?;
        Ok(())
    }
}
