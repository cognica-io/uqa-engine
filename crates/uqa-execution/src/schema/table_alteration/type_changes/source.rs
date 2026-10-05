//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconcile callback writes with the retained original row slots before rewriting the next relation.

use super::super::TableAlterContext;
use crate::schema::columns::rows::{changes, RewriteRow, RewriteRows};
use uqa_core::Value;
use uqa_sql::{ast::ColumnDef, SQLError};

pub(super) struct OriginalRows {
    pub columns: Vec<ColumnDef>,
    pub generation: [u8; 16],
    pub rows: RewriteRows,
}

impl OriginalRows {
    /// Completed callback writes belong to the next relation's input. A rolled-back callback leaves no row change beyond the retained transaction-log marker.
    pub(super) fn reconcile<S: Clone + 'static>(
        &mut self,
        context: &TableAlterContext<'_, S>,
        table: &str,
        marker: usize,
        memory: &uqa_core::memory::MemoryBudget,
    ) -> Result<(), SQLError> {
        let changed = changes::capture_since(
            context.row_changes,
            marker,
            self.generation,
            memory,
            context.columns.rewrite.cancellation,
        )?;
        if changed.is_empty() {
            return Ok(());
        }
        let current_columns = context
            .columns
            .analysis
            .state
            .stored_columns(table)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("rewrite source columns", error.as_ref())
            })?;
        let mut merged = RewriteRows::new(memory);
        let mut position = 0;
        let mut original = next_row(&mut self.rows, &mut position)?;
        changed.visit(&mut |id, present| {
            context.columns.rewrite.cancellation.check()?;
            while original.as_ref().is_some_and(|row| row.original_id < id) {
                let row = original
                    .take()
                    .expect("original row precedes changed identity");
                merged.push(row.original_id, row.document)?;
                original = next_row(&mut self.rows, &mut position)?;
            }
            if original.as_ref().is_some_and(|row| row.original_id == id) {
                original = next_row(&mut self.rows, &mut position)?;
            }
            if present {
                if let Some(document) = context.columns.rewrite.reads.raw_document(table, id)? {
                    merged.push(
                        id,
                        original_slots(&self.columns, &current_columns, &document)?,
                    )?;
                }
            }
            Ok(())
        })?;
        while let Some(row) = original.take() {
            context.columns.rewrite.cancellation.check()?;
            merged.push(row.original_id, row.document)?;
            original = next_row(&mut self.rows, &mut position)?;
        }
        self.rows = merged;
        Ok(())
    }
}

fn next_row(rows: &mut RewriteRows, position: &mut u64) -> Result<Option<RewriteRow>, SQLError> {
    if *position == rows.len() {
        return Ok(None);
    }
    let row = rows.get(*position)?;
    *position += 1;
    Ok(Some(row))
}

/// Names identify expressions, while immutable column identities identify slots. A new column reusing a dropped name cannot supply the dropped column's old slot.
fn original_slots(
    original: &[ColumnDef],
    current: &[ColumnDef],
    document: &uqa_storage::document_store::Document,
) -> Result<uqa_storage::document_store::Document, SQLError> {
    let mut source = original
        .iter()
        .filter(|column| {
            !column.generated.as_ref().is_some_and(|generated| {
                generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
            })
        })
        .map(|column| {
            let value = current
                .iter()
                .find(|candidate| {
                    column.object_id.is_some() && candidate.object_id == column.object_id
                })
                .and_then(|candidate| document.get(&candidate.name))
                .cloned()
                .unwrap_or(Value::Null);
            (column.name.clone(), value)
        })
        .collect();
    crate::query::generated::materialize_virtual_generated_columns(original, &mut source)?;
    Ok(source)
}
