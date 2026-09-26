//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind canonical build input to actual catalog records on the same fixed view.

use crate::diskann_index::catalog;
use uqa_core::memory::{Budgeted, BudgetedVec};

use super::{decode_value, relation_key, StoredCatalogIndex, TAG_CATALOG_INDEX, TAG_TABLE};
use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueBatch, RelationIdentity, StorageBackendResult, TableSchema};

pub(in crate::key_value) struct Binding {
    table: Record,
    index: Record,
    pub(in crate::key_value) parameters: DiskANNIndexParams,
}

struct Record {
    key: BudgetedVec<u8>,
    revision: KeyValueReadRevision,
}

impl Binding {
    pub(in crate::key_value) fn capture(
        read: &dyn KeyValueRead,
        table: &str,
        field: &str,
        dimensions: u32,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(crate::StorageBackendError::Other)?;
        if relation.qualified_name() != table || relation.schema != index.schema {
            return Err(invalid("index and canonical table names do not match"));
        }
        let table = Record::capture(read, TAG_TABLE, &relation, control)?;
        let schema: Budgeted<TableSchema> = decode(read, &table.key, control)?;
        if schema.relation != relation
            || schema.object_id == [0; 16]
            || schema.storage_generation == [0; 16]
        {
            return Err(invalid(
                "canonical field has no matching table owner and dimensions",
            ));
        }
        catalog::validate_field(&schema.vector_fields, field, dimensions)?;
        super::super::table_owners::validate_table(read, &schema)?;
        let index = Record::capture(read, TAG_CATALOG_INDEX, index, control)?;
        let definition: Budgeted<StoredCatalogIndex> = decode(read, &index.key, control)?;
        if definition.table_name != relation.qualified_name() {
            return Err(invalid("catalog index does not own this canonical field"));
        }
        let parameters = catalog::parameters(
            &definition.index_type,
            &definition.columns_json,
            &definition.parameters_json,
            field,
            dimensions,
            control,
        )?;
        control.check()?;
        Ok(Self {
            table,
            index,
            parameters,
        })
    }

    pub(in crate::key_value) fn prefixes(&self) -> [&[u8]; 2] {
        [&self.table.key, &self.index.key]
    }

    /// Recheck against the publication command's view and retain its committed preconditions in that same command's batch. This does not publish a generation or complete the caller's transaction.
    pub(in crate::key_value) fn require_current(
        &self,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        for record in [&self.table, &self.index] {
            control.check()?;
            if read.record_revision(&record.key)?.as_ref() != Some(&record.revision) {
                return Err(invalid(
                    "table or index definition changed since build capture",
                ));
            }
            batch.require_unchanged(&record.key)?;
        }
        control.check()
    }
}

impl Record {
    fn capture(
        read: &dyn KeyValueRead,
        tag: u8,
        relation: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let size = relation
            .schema
            .len()
            .checked_add(relation.name.len())
            .and_then(|size| size.checked_add(17))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let _encoding = control.memory().reserve(size)?;
        let encoded = relation_key(tag, relation)?;
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(&encoded)?;
        let revision = read
            .record_revision(&key)?
            .ok_or_else(|| invalid("catalog record is missing"))?;
        Ok(Self { key, revision })
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    read: &dyn KeyValueRead,
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<T>> {
    read.control().check()?;
    control.check()?;
    let mut result = None;
    let mut seen = false;
    let mut failed = false;
    let outcome = read.visit_value_bounded(key, control.memory().limit(), control, &mut |value| {
        if seen || failed {
            failed = true;
            return Err(invalid("catalog record returned repeatedly"));
        }
        seen = true;
        let decoded = (|| {
            let bytes = value.ok_or_else(|| invalid("catalog record is missing"))?;
            // Hold the decode allowance through validation of owned strings and collections.
            let size = bytes
                .len()
                .checked_mul(16)
                .and_then(|size| size.checked_add(std::mem::size_of::<T>()))
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            let memory = control.memory().reserve(size)?;
            Ok(Budgeted::new(decode_value(bytes)?, memory))
        })();
        match decoded {
            Ok(decoded) => result = Some(decoded),
            Err(error) => {
                failed = true;
                return Err(error);
            }
        }
        Ok(())
    });
    outcome?;
    read.control().check()?;
    control.check()?;
    if failed {
        return Err(invalid("catalog reader suppressed a failed completion"));
    }
    result.ok_or_else(|| invalid("catalog record was not returned"))
}

fn invalid(message: &str) -> crate::StorageBackendError {
    crate::StorageBackendError::Other(format!("invalid DiskANN catalog binding: {message}"))
}

#[cfg(test)]
mod tests;
