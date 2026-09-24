//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit restoration allowance and storage-backed index validation for the fixture.

use super::*;
use uqa_core::DocId;
use uqa_sql::{ast::TableHierarchy, SQLError};
use uqa_storage::{
    document_store::Document,
    key_value::{KeyValueMutation, KeyValueReadScope},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    KeyValueBatch, MemoryKeyValueStore,
};

pub(super) struct ControlledStore {
    inner: MemoryKeyValueStore,
    control: StorageReadControl,
}

impl ControlledStore {
    pub(super) fn new() -> Self {
        Self {
            inner: MemoryKeyValueStore::new(),
            control: StorageReadControl::with_limit(1 << 20),
        }
    }
}

impl KeyValueStore for ControlledStore {
    fn retention_control(&self) -> Option<StorageReadControl> {
        Some(self.control.clone())
    }
    fn with_read_view(&self, read: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        self.inner.with_read_view(read)
    }
    fn with_mutation(&self, mutate: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        self.inner.with_mutation(mutate)
    }
    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_value(key, control, visit)
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner
            .visit_prefix_after(prefix, after, limit, control, visit)
    }
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.inner.put(key, value)
    }
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.inner.delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix)
    }
    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.inner.delete_prefix(prefix)
    }
    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        self.inner.batch()
    }
    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_transaction()
    }
    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }
    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.inner.commit_transaction()
    }
    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.inner.rollback_transaction()
    }
}

impl crate::schema::indexes::IndexBuildCatalog for Fixture {
    fn table_hierarchy(&self, table: &str) -> Result<TableHierarchy, SQLError> {
        assert_eq!(table, TABLE);
        Ok(TableHierarchy::default())
    }
    fn scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        Ok(vec![table.into()])
    }
}

impl crate::mutation::constraints::context::MutationRead for Fixture {
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.backend
            .document_store(table)
            .doc_ids()
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
    fn live_table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.table_doc_ids(table)
    }
    fn get_document(&self, table: &str, id: DocId) -> Result<Option<Document>, SQLError> {
        self.backend
            .document_store(table)
            .get(id)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
    fn command_overlay_changed_ids(&self, _: &str) -> Result<Option<BTreeSet<DocId>>, SQLError> {
        Ok(None)
    }
}

impl uqa_sql::semantics::conflict::ConflictCatalog for Fixture {
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        assert_eq!(table, TABLE);
        let stored = self
            .catalog
            .load_tables()
            .map_err(|error| error.to_string())?;
        serde_json::from_str(&stored[0].columns_json)
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<uqa_sql::catalog::index::EnforcedKey>, String> {
        unreachable!("index builds read the stored declaration")
    }
    fn try_declared_table_constraints(
        &self,
        _: &str,
    ) -> Result<uqa_sql::ast::TableConstraintSet, String> {
        unreachable!("index builds read the stored declaration")
    }
}

impl uqa_sql::semantics::partition::PartitionExpressions for Fixture {
    fn evaluate_bound(
        &self,
        _: &uqa_sql::ast::Expr,
        _: &[uqa_sql::SQLParam],
    ) -> Result<Value, SQLError> {
        unreachable!("the fixture has no partition bounds")
    }
    fn evaluate_row(
        &self,
        expression: &uqa_sql::ast::Expr,
        row: &Document,
        _: &uqa_sql::RowSchema,
        _: &[uqa_sql::SQLParam],
    ) -> Result<Value, SQLError> {
        match expression {
            uqa_sql::ast::Expr::Column(column) => Ok(row[column].clone()),
            _ => unreachable!("the fixture uses column expressions"),
        }
    }
}

impl crate::query::runtime::QueryMemorySettings for Fixture {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Ok(1 << 20)
    }
}
