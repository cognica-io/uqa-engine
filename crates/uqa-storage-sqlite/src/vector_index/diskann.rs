//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native canonical tensors and origins share one evaluated publishing mutation.

mod lifecycle;
mod live;
mod maintenance;
mod retained;
#[cfg(test)]
mod tests;

pub use live::SQLiteDiskANNHandle;
pub use retained::RetainedSQLiteDiskANNCanonical;

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    diskann_index::format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
    mvcc::VersionError,
    read_control::StorageReadControl,
    vector_index::validate_vector_values_controlled,
    StorageBackendResult,
};

use super::{encode_doc_id, SQLiteVectorIndex};
use crate::{mvcc::native::NativeRecordFamily as Family, ManagedConnection};

/// Canonical tensor owner for a bound native `SQLite` session. Raw coordinates stay in `_vectors`; this does not enable a public `DiskANN` index or publish a generation.
pub struct SQLiteDiskANNCanonical {
    index: SQLiteVectorIndex,
}

impl SQLiteDiskANNCanonical {
    pub fn new(
        connection: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 || !connection.is_native_record_session() {
            return Err(invalid(
                "canonical origins require dimensions and native records",
            ));
        }
        Ok(Self {
            index: SQLiteVectorIndex::new(connection, table, field, dimensions),
        })
    }

    /// Replace every ordinal and the complete tensor's publishing origin in one batch, including explicit empty replacements. The callback is never replayed after a publication failure.
    pub fn replace(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.replace_guarded(document, vectors, control, |_, _| Ok(()))
    }

    fn replace_guarded(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &StorageReadControl,
        guard: impl FnOnce(
            &crate::mvcc::native::NativeSnapshot,
            &mut dyn uqa_storage::KeyValueBatch,
        ) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        control.check()?;
        let document = encode_doc_id(document)?;
        let count =
            u64::try_from(vectors.len()).map_err(|_| invalid("canonical count overflow"))?;
        super::validate_vector_ordinal_count(count)?;
        for vector in vectors {
            validate_vector_values_controlled(self.index.dimensions, vector, Some(control))?;
        }
        Ok(self
            .index
            .conn
            .with_native_versioned_write(|origin, snapshot, batch| {
                control.check()?;
                guard(snapshot, batch)?;
                let owner = snapshot.ensure_table_owner(&self.index.table, batch)?;
                let field = ValueRef::Text(self.index.field.as_bytes());
                snapshot.coordinate_vector_field(batch, owner, field, false)?;
                let version = DiskANNVectorVersion::new(origin.transaction(), origin.revision())?;
                let record = DiskANNCanonicalOrigin::new(version, self.index.dimensions, count)?;
                snapshot.delete_prefix(
                    batch,
                    Family::Vectors,
                    owner,
                    &[field, ValueRef::Integer(document)],
                )?;
                let mut bytes = BudgetedVec::new(control.memory());
                for (ordinal, vector) in vectors.iter().enumerate() {
                    control.check()?;
                    bytes.clear();
                    for chunk in vector.chunks(1024) {
                        control.check()?;
                        for coordinate in chunk {
                            bytes.extend_from_slice(&coordinate.to_le_bytes())?;
                        }
                    }
                    snapshot.put_row(
                        batch,
                        Family::Vectors,
                        owner,
                        &[
                            ValueRef::Text(self.index.table.as_bytes()),
                            field,
                            ValueRef::Integer(document),
                            ValueRef::Integer(ordinal as i64),
                            ValueRef::Blob(&bytes),
                        ],
                    )?;
                }
                snapshot.put_row(
                    batch,
                    Family::VectorOrigins,
                    owner,
                    &[
                        ValueRef::Text(self.index.table.as_bytes()),
                        field,
                        ValueRef::Integer(document),
                        ValueRef::Blob(&record.encode()),
                    ],
                )?;
                snapshot.put_row(
                    batch,
                    Family::VectorChanges,
                    owner,
                    &[
                        ValueRef::Text(self.index.table.as_bytes()),
                        field,
                        ValueRef::Blob(
                            &DiskANNChangeIdentity::new(document as DocId, version).encode(),
                        ),
                        ValueRef::Blob(&record.encode()),
                    ],
                )?;
                control.check()?;
                Ok(version)
            })?)
    }

    /// Retain the exact private/committed source and table incarnation without reading the vector corpus.
    pub fn retain(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedSQLiteDiskANNCanonical> {
        RetainedSQLiteDiskANNCanonical::capture(&self.index, control)
    }

    /// Capture the actual native table-name owner, table definition and index definition on the canonical source's fixed view.
    pub fn retain_for_index(
        &self,
        index: &uqa_storage::RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedSQLiteDiskANNCanonical> {
        RetainedSQLiteDiskANNCanonical::capture_for_index(&self.index, index, control)
    }
}

fn invalid(message: &'static str) -> uqa_storage::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}
