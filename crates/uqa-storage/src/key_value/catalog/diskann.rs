//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind canonical build input to actual catalog records on the same fixed view.

use crate::diskann_index::catalog::{self, DiskANNCatalogRecord};
use uqa_core::memory::{Budgeted, BudgetedVec};

use super::{decode_value, relation_key, StoredCatalogIndex, TAG_CATALOG_INDEX, TAG_TABLE};
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueBatch, RelationIdentity, StorageBackendResult, TableSchema};

pub(in crate::key_value) struct Binding {
    table: DiskANNCatalogRecord,
    index: DiskANNCatalogRecord,
    pub(in crate::key_value) parameters: DiskANNIndexParams,
}

impl Binding {
    pub(in crate::key_value) fn scope(
        &self,
        read: &dyn KeyValueRead,
        resolver: &dyn catalog::DiskANNIndexResolver,
        capture: &StorageReadControl,
        control: &StorageReadControl,
    ) -> StorageBackendResult<catalog::DiskANNIndexScope> {
        let table: Budgeted<TableSchema> = decode(read, self.table.key(), control)?;
        let index: Budgeted<StoredCatalogIndex> = decode(read, self.index.key(), control)?;
        catalog::resolve_scope(
            resolver,
            (table.object_id, table.storage_generation),
            index.definition_json.as_deref(),
            self.index.revision(),
            read.control(),
            capture,
            control,
        )
    }

    pub(in crate::key_value) fn capture(
        read: &dyn KeyValueRead,
        table: &str,
        field: &str,
        dimensions: u32,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let relation = catalog::bound_relation(table, index, || {
            invalid("index and canonical table names do not match")
        })?;
        let table = capture_record(read, TAG_TABLE, &relation, control)?;
        let schema: Budgeted<TableSchema> = decode(read, table.key(), control)?;
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
        let index = capture_record(read, TAG_CATALOG_INDEX, index, control)?;
        let definition: Budgeted<StoredCatalogIndex> = decode(read, index.key(), control)?;
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
        [self.table.key(), self.index.key()]
    }

    /// Recheck against the publication command's view and retain its committed preconditions in that same command's batch. This does not publish a generation or complete the caller's transaction.
    pub(in crate::key_value) fn require_current(
        &self,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        catalog::require_current_records([&self.table, &self.index], read, batch, control, || {
            invalid("table or index definition changed since build capture")
        })
    }
}

fn capture_record(
    read: &dyn KeyValueRead,
    tag: u8,
    relation: &RelationIdentity,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNCatalogRecord> {
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
    DiskANNCatalogRecord::capture(read, key, || invalid("catalog record is missing"))
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
