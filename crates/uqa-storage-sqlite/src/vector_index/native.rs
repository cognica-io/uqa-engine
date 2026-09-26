//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical vectors on a single committed/private native record boundary.

use std::sync::Arc;

use rusqlite::types::ValueRef;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryReservation},
    DocId,
};
use uqa_storage::{mvcc::VersionError, KeyValueBatch};

use super::{
    blob_to_vector, decode_doc_id, validate_persisted_ordinal_sequence, SQLiteVectorIndex,
};
use crate::mvcc::native::{
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot,
};
use crate::{Result, SQLiteError};

pub(in crate::vector_index) mod publication;
pub(in crate::vector_index) mod records;

type VectorRows = Vec<(DocId, u32, Vec<f32>)>;

// Payload allocations are destroyed before either of their reservations, including on failed reads.
pub(super) struct VectorBuffer<T> {
    pub(super) rows: BudgetedVec<T>,
    pub(super) payload: MemoryReservation,
}

impl<T> VectorBuffer<T> {
    pub(super) fn new(read: &NativeVectorRead<'_>) -> Result<Self> {
        Ok(Self {
            rows: BudgetedVec::new(read.snapshot.control.memory()),
            payload: read.snapshot.control.memory().reserve(0)?,
        })
    }

    pub(super) fn finish(self) -> Budgeted<Vec<T>> {
        let (rows, mut memory) = self.rows.into_parts();
        memory.absorb(self.payload);
        Budgeted::new(rows, memory)
    }
}

pub(super) fn integer(value: ValueRef<'_>) -> Result<i64> {
    value
        .as_i64()
        .map_err(|_| SQLiteError::StorageBackend("invalid native vector integer".into()))
}

pub(super) fn blob(value: ValueRef<'_>) -> Result<&[u8]> {
    value
        .as_blob()
        .map_err(|_| SQLiteError::StorageBackend("invalid native vector blob".into()))
}

pub(super) fn text(value: ValueRef<'_>) -> Result<&str> {
    value
        .as_str()
        .map_err(|_| SQLiteError::StorageBackend("invalid native vector text".into()))
}

pub(super) struct NativeVectorRead<'a> {
    pub(super) snapshot: &'a NativeSnapshot,
    pub(super) index: &'a SQLiteVectorIndex,
    pub(super) owner: Option<NativeRecordOwner>,
}

impl SQLiteVectorIndex {
    pub(super) fn native_snapshot(&self) -> Result<Option<Arc<NativeSnapshot>>> {
        match &self.retained {
            Some(snapshot) => Ok(Some(Arc::clone(snapshot))),
            None => self.conn.native_snapshot(),
        }
    }

    pub(super) fn retained_snapshot(&self) -> Result<Self> {
        let mut snapshot = self.clone();
        snapshot.retained = self.native_snapshot()?;
        Ok(snapshot)
    }

    pub(super) fn read_native<R>(
        &self,
        read: impl FnOnce(&NativeVectorRead<'_>) -> Result<R>,
    ) -> Result<Option<R>> {
        self.native_snapshot()?
            .map(|snapshot| read(&NativeVectorRead::new(&snapshot, self)?))
            .transpose()
    }

    pub(super) fn write_native<R>(
        &self,
        write: impl FnOnce(&NativeVectorRead<'_>, &mut dyn KeyValueBatch) -> Result<R>,
    ) -> Result<Option<R>> {
        if self.retained.is_some() {
            return Err(SQLiteError::StorageBackend(
                "a retained vector snapshot is read-only".into(),
            ));
        }
        self.conn.with_native_write(|snapshot, batch| {
            write(&NativeVectorRead::new(snapshot, self)?, batch)
        })
    }
}

impl<'a> NativeVectorRead<'a> {
    pub(super) fn new(snapshot: &'a NativeSnapshot, index: &'a SQLiteVectorIndex) -> Result<Self> {
        Ok(Self {
            snapshot,
            index,
            owner: snapshot.table_owner(&index.table)?,
        })
    }

    pub(super) fn field(&self) -> ValueRef<'_> {
        ValueRef::Text(self.index.field.as_bytes())
    }

    pub(super) fn owned(&self, batch: &mut dyn KeyValueBatch) -> Result<Self> {
        let owner = match self.owner {
            Some(owner) => owner,
            None => self.snapshot.ensure_table_owner(&self.index.table, batch)?,
        };
        Ok(Self {
            snapshot: self.snapshot,
            index: self.index,
            owner: Some(owner),
        })
    }

    pub(super) fn vectors(&self) -> Result<Budgeted<VectorRows>> {
        let mut output = VectorBuffer::new(self)?;
        let (rows, payload) = (&mut output.rows, &mut output.payload);
        if let Some(owner) = self.owner {
            self.snapshot
                .visit_rows(Family::Vectors, Some(owner), &[self.field()], |row| {
                    let blob = blob(row[4])?;
                    payload.grow(blob.len())?;
                    rows.reserve(1)?;
                    let vector = blob_to_vector(blob)?;
                    self.index.validate_dimensions_sqlite(&vector)?;
                    let ordinal = u32::try_from(integer(row[3])?).map_err(|_| {
                        SQLiteError::StorageBackend("invalid native vector ordinal".into())
                    })?;
                    rows.push((decode_doc_id(integer(row[2])?)?, ordinal, vector))?;
                    Ok(())
                })?;
        }
        validate_persisted_ordinal_sequence(rows)?;
        Ok(output.finish())
    }

    pub(super) fn count(&self) -> Result<usize> {
        let Some(owner) = self.owner else {
            return Ok(0);
        };
        let prefix = NativeRecordIdentity::new(Family::Vectors, owner)?
            .encode_prefix(&[self.field()], &self.snapshot.control)?;
        let mut count = 0_usize;
        self.snapshot.view.visit_keys(
            &prefix,
            None,
            usize::MAX,
            &self.snapshot.control,
            &mut |_, record| {
                if record.live {
                    count = count.checked_add(1).ok_or(VersionError::InvalidEncoding(
                        "native vector count overflow",
                    ))?;
                }
                Ok(true)
            },
        )?;
        Ok(count)
    }

    pub(super) fn contains_document(&self, doc_id: i64) -> Result<bool> {
        let Some(owner) = self.owner else {
            return Ok(false);
        };
        self.snapshot.contains_row(
            Family::Vectors,
            owner,
            &[
                self.field(),
                ValueRef::Integer(doc_id),
                ValueRef::Integer(0),
            ],
        )
    }

    pub(super) fn replace(
        &self,
        batch: &mut dyn KeyValueBatch,
        doc_id: i64,
        vectors: &[(i64, Vec<u8>)],
    ) -> Result<()> {
        if vectors.is_empty() {
            return self.delete(batch, doc_id);
        }
        let owner = match self.owner {
            Some(owner) => owner,
            None => self.snapshot.ensure_table_owner(&self.index.table, batch)?,
        };
        self.snapshot.delete_prefix(
            batch,
            Family::VectorOrigins,
            owner,
            &[self.field(), ValueRef::Integer(doc_id)],
        )?;
        self.snapshot.delete_prefix(
            batch,
            Family::Vectors,
            owner,
            &[self.field(), ValueRef::Integer(doc_id)],
        )?;
        for (ordinal, vector) in vectors {
            self.snapshot.put_row(
                batch,
                Family::Vectors,
                owner,
                &[
                    ValueRef::Text(self.index.table.as_bytes()),
                    self.field(),
                    ValueRef::Integer(doc_id),
                    ValueRef::Integer(*ordinal),
                    ValueRef::Blob(vector),
                ],
            )?;
        }
        Ok(())
    }

    pub(super) fn delete(&self, batch: &mut dyn KeyValueBatch, doc_id: i64) -> Result<()> {
        if let Some(owner) = self.owner {
            self.snapshot.delete_prefix(
                batch,
                Family::VectorOrigins,
                owner,
                &[self.field(), ValueRef::Integer(doc_id)],
            )?;
            self.snapshot.delete_prefix(
                batch,
                Family::Vectors,
                owner,
                &[self.field(), ValueRef::Integer(doc_id)],
            )?;
        }
        Ok(())
    }

    pub(super) fn clear_family(&self, batch: &mut dyn KeyValueBatch, family: Family) -> Result<()> {
        if let Some(owner) = self.owner {
            if family == Family::Vectors {
                for related in [Family::VectorOrigins, Family::VectorChanges] {
                    self.snapshot
                        .delete_prefix(batch, related, owner, &[self.field()])?;
                }
            }
            self.snapshot
                .delete_prefix(batch, family, owner, &[self.field()])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Catalog, ManagedConnection, SQLiteHNSWIndex, SQLiteIVFIndex, SQLiteVectorIndex};
    use uqa_storage::{mvcc::VersionedSessionOptions, vector_index::VectorIndex};

    #[test]
    fn native_vector_mutations_reject_read_only_record_transactions() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let indexes: [Box<dyn VectorIndex>; 3] = [
            Box::new(SQLiteVectorIndex::new(
                connection.clone(),
                "docs",
                "exact",
                3,
            )),
            Box::new(SQLiteIVFIndex::new(connection.clone(), "docs", "ivf", 3)),
            Box::new(SQLiteHNSWIndex::new(connection.clone(), "docs", "hnsw", 3)),
        ];
        for mut index in indexes {
            connection.begin_record_read().unwrap();
            assert!(index.add(1, vec![1.0, 0.0, 0.0]).is_err());
            assert!(!connection.transaction_has_written().unwrap());
            connection.commit_transaction().unwrap();
            assert_eq!(index.count().unwrap(), 0);
        }
    }
}
