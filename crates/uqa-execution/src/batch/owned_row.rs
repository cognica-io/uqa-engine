//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned physical rows used by row-at-a-time consumers.

use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::ResultRow;

use crate::physical::{ExecError, ExecResult};

use super::{PhysicalRow, PhysicalRowView, RowSchema};

/// Owned schema/row pair for row-at-a-time consumers that must outlive a decoded batch. Cloning this carrier shares the immutable schema index and row fragments; it does not build a named row or clone contained values.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedPhysicalRow {
    pub schema: RowSchema,
    pub row: PhysicalRow,
}

impl OwnedPhysicalRow {
    pub fn new(schema: RowSchema, row: PhysicalRow) -> Self {
        Self { schema, row }
    }

    pub fn view(&self) -> PhysicalRowView<'_> {
        self.schema.view(&self.row)
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.schema
            .exact_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    /// Read one flattened executor slot without introducing a SQL name.
    pub fn physical_value_at(&self, position: usize) -> Option<&Value> {
        self.row.value(position)
    }

    /// Apply a new logical schema by position while sharing the existing value fragments. Relation aliases and derived-column names therefore do not require an intermediate named row.
    pub fn relabel(self, schema: RowSchema) -> ExecResult<Self> {
        if self.schema.len() != schema.len() {
            return Err(ExecError::Other(format!(
                "cannot relabel {} columns as {} columns",
                self.schema.len(),
                schema.len()
            )));
        }
        let slots = self.schema.index.slots.to_vec();
        Ok(Self::new(schema, self.row.project_slots(&slots)))
    }

    pub fn into_result_row(self) -> ResultRow {
        self.schema.materialize_result_row(self.row)
    }
}

impl RowLookup for OwnedPhysicalRow {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .column_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column)
            .and_then(|slot| self.row.value(slot))
    }

    fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.schema.qualified_column_is_ambiguous(qualifier, column)
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.schema
            .slot(index)
            .and_then(|slot| self.row.value(slot))
    }

    fn internal_column(&self, column: uqa_sql::ast::InternalColumnRef) -> Option<&Value> {
        self.schema
            .internal_slot(column)
            .and_then(|slot| self.row.value(slot))
    }

    fn score_source(&self, qualifier: Option<&str>) -> Option<&Value> {
        self.schema
            .score_source_slot(qualifier)
            .and_then(|slot| self.row.value(slot))
    }

    fn score_source_is_ambiguous(&self, qualifier: Option<&str>) -> bool {
        self.schema.score_source_is_ambiguous(qualifier)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.view().visit_columns(visitor);
    }
}
