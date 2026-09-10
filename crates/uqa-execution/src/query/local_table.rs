//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical scans over persisted rows, generated fields, recheck pins, and command overlays.

use crate::RowSchemaExecution;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{plan::source_projection::RelationMetadataProjection, ResultRow, SQLError};

mod command_scan;
mod row_source;

pub type SharedLockOrigin = (Arc<str>, Arc<str>);

pub struct LocalTableRowSource {
    cancellation: uqa_core::CancellationToken,
    table_name: String,
    table: Arc<dyn super::table_read::TableRead>,
    column_definitions: Arc<Vec<uqa_sql::ast::ColumnDef>>,
    columns: Vec<String>,
    schema: Vec<String>,
    physical_schema: crate::RowSchema,
    metadata: RelationMetadataProjection,
    table_oid: Option<Value>,
    predicate: Option<crate::ProjectedPredicate>,
    estimated_cardinality: u64,
    after: Option<uqa_core::DocId>,
    lock_origin: Option<SharedLockOrigin>,
    recheck_pins: Option<Arc<Vec<crate::row_locks::recheck::RecheckDoc>>>,
    recheck_cursor: usize,
    command_changes: Option<
        Arc<std::collections::BTreeMap<uqa_core::DocId, Option<uqa_storage::StoredDocument>>>,
    >,
    command_change_after: Option<uqa_core::DocId>,
    command_base_after: Option<uqa_core::DocId>,
    command_base_ids: std::collections::VecDeque<uqa_core::DocId>,
    command_base_exhausted: bool,
}

/// One logical inheritance scan over independently stored physical tables.
/// Each child source retains its own lock origin and command overlay while the
/// SQL-visible row type remains the selected ancestor's row type.
pub struct HierarchyRowSource {
    sources: std::collections::VecDeque<LocalTableRowSource>,
    schema: Vec<String>,
    physical_schema: crate::RowSchema,
    estimated_cardinality: u64,
}

impl HierarchyRowSource {
    pub fn new(sources: Vec<LocalTableRowSource>) -> Result<Self, SQLError> {
        let first = sources.first().ok_or_else(|| {
            SQLError::Internal("inheritance scan was built without a physical table".into())
        })?;
        let schema = first.schema.clone();
        let physical_schema = first.physical_schema.clone();
        let estimated_cardinality = sources
            .iter()
            .map(|source| source.estimated_cardinality)
            .sum();
        Ok(Self {
            sources: sources.into(),
            schema,
            physical_schema,
            estimated_cardinality,
        })
    }
}

impl crate::RowSource for HierarchyRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&crate::RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> crate::ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> crate::ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_batch(max_rows)?;
        Ok(rows
            .iter()
            .map(|row| self.physical_schema.view(row).to_result_row())
            .collect())
    }

    fn next_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let Some(source) = self.sources.front_mut() else {
                break;
            };
            let mut batch = source.next_physical_rows_batch(max_rows - rows.len())?;
            if batch.is_empty() {
                self.sources.pop_front();
            } else {
                rows.append(&mut batch);
            }
        }
        Ok(rows)
    }
}

pub struct LocalTableScanConfig {
    pub cancellation: uqa_core::CancellationToken,
    pub table_name: String,
    pub table: Arc<dyn super::table_read::TableRead>,
    pub column_definitions: Arc<Vec<uqa_sql::ast::ColumnDef>>,
    pub columns: Vec<String>,
    pub schema: Vec<String>,
    pub physical_schema: crate::RowSchema,
    pub metadata: RelationMetadataProjection,
    pub table_oid: Option<Value>,
    pub predicate: Option<crate::ProjectedPredicate>,
    pub estimated_cardinality: u64,
    pub lock_origin: Option<SharedLockOrigin>,
    pub recheck_pins: Option<Arc<Vec<crate::row_locks::recheck::RecheckDoc>>>,
    pub command_changes: Option<
        Arc<std::collections::BTreeMap<uqa_core::DocId, Option<uqa_storage::StoredDocument>>>,
    >,
}

impl LocalTableRowSource {
    pub fn new(config: LocalTableScanConfig) -> Self {
        Self {
            cancellation: config.cancellation,
            table_name: config.table_name,
            table: config.table,
            column_definitions: config.column_definitions,
            columns: config.columns,
            schema: config.schema,
            physical_schema: config.physical_schema,
            metadata: config.metadata,
            table_oid: config.table_oid,
            predicate: config.predicate,
            estimated_cardinality: config.estimated_cardinality,
            lock_origin: config.lock_origin,
            recheck_pins: config.recheck_pins,
            command_changes: config.command_changes,
            after: None,
            recheck_cursor: 0,
            command_change_after: None,
            command_base_after: None,
            command_base_ids: std::collections::VecDeque::new(),
            command_base_exhausted: false,
        }
    }
}

pub use row_source::table_lock_origin;
