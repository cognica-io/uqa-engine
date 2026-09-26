//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog selection stays on the canonical snapshot and guards its real physical records.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    diskann_index::catalog,
    key_value::{KeyValueRead, KeyValueReadRevision},
    mvcc::VersionError,
    read_control::StorageReadControl,
    vector_index::DiskANNIndexParams,
    KeyValueBatch, RelationIdentity, StorageBackendError, StorageBackendResult, VectorFieldSchema,
};

use crate::mvcc::native::{
    decode_record, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner as Owner, NativeSnapshot,
};

pub(crate) struct DiskANNCatalogBinding {
    records: [Record; 3],
    pub(crate) owner: Owner,
    pub(crate) parameters: DiskANNIndexParams,
}

struct Record {
    key: BudgetedVec<u8>,
    identity: Identity,
    revision: KeyValueReadRevision,
}

impl DiskANNCatalogBinding {
    pub(crate) fn capture(
        snapshot: &NativeSnapshot,
        table: &str,
        field: &str,
        dimensions: u32,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        snapshot.control.check()?;
        control.check()?;
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        if relation.qualified_name() != table || relation.schema != index.schema {
            return Err(invalid("index and canonical table names do not match"));
        }
        let database = Owner::Database(snapshot.database);
        let name = Record::capture(
            snapshot,
            Family::TableOwners,
            database,
            &[ValueRef::Text(table.as_bytes())],
            control,
        )?;
        let owner = name.read(snapshot, control, |row| {
            if text(row[0])? != table || row[3] != ValueRef::Integer(1) {
                return Err(invalid("canonical table has no catalog owner"));
            }
            let owner = Owner::Object {
                identity: identity(row[1])?,
                generation: identity(row[2])?,
            };
            Identity::new(Family::Tables, owner).map_err(VersionError::into_storage_error)?;
            Ok(owner)
        })?;
        let table_record = Record::capture(snapshot, Family::Tables, owner, &[], control)?;
        table_record.read(snapshot, control, |row| {
            validate_table(row, &relation, owner, field, dimensions, control)
        })?;
        let index_record = Record::capture(
            snapshot,
            Family::CatalogIndexes,
            database,
            &[
                ValueRef::Text(index.schema.as_bytes()),
                ValueRef::Text(index.name.as_bytes()),
            ],
            control,
        )?;
        let parameters = index_record.read(snapshot, control, |row| {
            if text(row[0])? != index.schema
                || text(row[1])? != index.name
                || text(row[2])? != "index"
                || text(row[4])? != relation.schema
                || text(row[5])? != relation.name
            {
                return Err(invalid(
                    "native index does not belong to the canonical table",
                ));
            }
            catalog::parameters(
                text(row[3])?,
                text(row[6])?,
                text(row[7])?,
                field,
                dimensions,
                control,
            )
        })?;
        control.check()?;
        Ok(Self {
            records: [name, table_record, index_record],
            owner,
            parameters,
        })
    }

    pub(crate) fn require_current(
        &self,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        for record in &self.records {
            control.check()?;
            if read.record_revision(&record.key)?.as_ref() != Some(&record.revision) {
                return Err(invalid("table or index changed since build capture"));
            }
            batch.require_unchanged(&record.key)?;
        }
        control.check()
    }
}

impl Record {
    fn capture(
        snapshot: &NativeSnapshot,
        family: Family,
        owner: Owner,
        components: &[ValueRef<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let identity = Identity::new(family, owner).map_err(VersionError::into_storage_error)?;
        let key = identity
            .encode_key(components, control)
            .map_err(VersionError::into_storage_error)?;
        let revision = snapshot
            .record_read()
            .record_revision(&key)?
            .ok_or_else(|| invalid("native catalog record is missing"))?;
        Ok(Self {
            key,
            identity,
            revision,
        })
    }

    fn read<T>(
        &self,
        snapshot: &NativeSnapshot,
        control: &StorageReadControl,
        mut read: impl FnMut(&[ValueRef<'_>]) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        snapshot.control.check()?;
        control.check()?;
        let mut result = None;
        snapshot.record_read().visit_value_bounded(
            &self.key,
            control.memory().limit(),
            control,
            &mut |value| {
                let bytes = value.ok_or_else(|| invalid("native catalog record is missing"))?;
                let (identity, row) = decode_record(&self.key, bytes, control)
                    .map_err(VersionError::into_storage_error)?;
                if identity != self.identity {
                    return Err(invalid("native catalog record has another owner"));
                }
                result = Some(read(&row)?);
                Ok(())
            },
        )?;
        snapshot.control.check()?;
        control.check()?;
        result.ok_or_else(|| invalid("native catalog record was not returned"))
    }
}

fn validate_table(
    row: &[ValueRef<'_>],
    relation: &RelationIdentity,
    owner: Owner,
    field: &str,
    dimensions: u32,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    if text(row[0])? != relation.schema
        || text(row[1])? != relation.name
        || text(row[2])? != "table"
        || owner
            != (Owner::Object {
                identity: identity(row[9])?,
                generation: identity(row[8])?,
            })
    {
        return Err(invalid("native table definition disagrees with its owner"));
    }
    let encoded = text(row[5])?;
    let _memory = control.memory().reserve(
        encoded
            .len()
            .checked_mul(16)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
    )?;
    let fields: Vec<VectorFieldSchema> =
        serde_json::from_str(encoded).map_err(|error| invalid(&error.to_string()))?;
    catalog::validate_field(&fields, field, dimensions)
}

fn text(value: ValueRef<'_>) -> StorageBackendResult<&str> {
    value
        .as_str()
        .map_err(|_| invalid("native catalog field is not text"))
}

fn identity(value: ValueRef<'_>) -> StorageBackendResult<[u8; 16]> {
    value
        .as_blob()
        .ok()
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| invalid("native catalog identity is not 16 bytes"))
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid native DiskANN catalog binding: {message}"))
}
