//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite a table's rows as `ATRewriteTable` does: recompute generated columns, check the new rows, remap physical primary keys, and validate the foreign keys involved.
use super::rows::RewriteRows;
use crate::mutation::{
    assignment::MutationAssignmentContext, constraints::context::ConstraintContext,
    publication::MutationStorage,
};
use crate::schema::keys::KeyValidationContext;
use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::StorageBackendResult;
pub trait GeneratedRewriteState {
    fn advance_next_id(&self, table: &str, id: DocId) -> StorageBackendResult<()>;
}
pub struct GeneratedRewriteContext<'a, S: Clone + 'static> {
    pub cancellation: &'a uqa_core::CancellationToken,
    pub keys: KeyValidationContext<'a>,
    pub assignment: MutationAssignmentContext<'a, S>,
    pub storage: &'a dyn MutationStorage,
    pub state: &'a dyn GeneratedRewriteState,
    pub identifiers: &'a dyn crate::mutation::identity::MutationIdentifiers,
}
/// Recompute the stored generated columns of every row of `table` and, when `rewrite_physical_rows`, rewrite the table with them, as adding a stored generated column or changing its expression does.
pub fn validate_and_rewrite_generated_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    rewrite_physical_rows: bool,
) -> Result<(), SQLError> {
    let allowance = context.keys.constraints.memory.work_mem_bytes()?;
    let memory = uqa_core::memory::MemoryBudget::new(allowance / 2);
    let read_memory = uqa_core::memory::MemoryBudget::new(allowance - allowance / 2);
    let control =
        uqa_storage::read_control::StorageReadControl::new(&read_memory, context.cancellation);
    let mut original =
        super::rows::capture(context.keys.constraints.reads, table, &memory, &control)?;
    let mut rows = RewriteRows::new(&memory);
    for position in 0..original.len() {
        context.cancellation.check()?;
        let super::rows::RewriteRow {
            original_id: doc_id,
            mut document,
            ..
        } = original.get(position)?;
        crate::mutation::assignment::refresh_stored_generated_columns(
            context.assignment,
            table,
            &mut document,
        )?;
        rows.push(doc_id, document)?;
    }
    drop(original);
    let changed = stored_generated_columns(context.keys.constraints, table)?;
    rewrite_table_rows(context, table, rows, rewrite_physical_rows, &changed)
}

/// Replace the rows of `table` with the rows a rewrite produced, as `ATRewriteTable` does. Each new row is checked against the table's validated NOT NULL and CHECK constraints, then the key constraints are checked across all of them, as rebuilding their indexes does; when `write`, the rows replace the old ones, a row whose integer primary key changed moving to the identity that key names. The foreign keys that involve a `changed` column are validated against the result.
pub fn rewrite_table_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    mut rows: RewriteRows,
    write: bool,
    changed: &[String],
) -> Result<(), SQLError> {
    for position in 0..rows.len() {
        context.cancellation.check()?;
        let row = rows.get(position)?;
        crate::mutation::constraints::validate_rewritten_row(
            context.keys.constraints,
            table,
            &row.document,
        )?;
    }
    super::super::keys::validate_key_constraint_rows(&context.keys, table, &mut rows)?;
    if write {
        publish_rewritten_rows(context, table, rows)?;
        super::super::keys::validate_temporal_key_rows(&context.keys, table)?;
    }
    crate::schema::validation::validate_rewritten_foreign_keys(
        context.keys.constraints,
        table,
        changed,
    )
}

fn publish_rewritten_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    mut rows: RewriteRows,
) -> Result<(), SQLError> {
    let mut replacements = RewriteRows::new(rows.memory());
    let mut remaps_primary_key = false;
    for position in 0..rows.len() {
        let super::rows::RewriteRow {
            original_id: old_doc_id,
            document,
            ..
        } = rows.get(position)?;
        let new_doc_id = crate::mutation::identity::key_relocation(
            context.keys.constraints.catalog,
            context.identifiers,
            table,
            old_doc_id,
            &document,
        )?
        .unwrap_or(old_doc_id);
        remaps_primary_key |= new_doc_id != old_doc_id;
        replacements.push_replacement(old_doc_id, new_doc_id, document)?;
    }
    drop(rows);
    if remaps_primary_key {
        for position in 0..replacements.len() {
            context
                .storage
                .delete_document(table, replacements.get(position)?.original_id)?;
        }
    }
    for position in 0..replacements.len() {
        context.cancellation.check()?;
        let super::rows::RewriteRow {
            original_id: old_doc_id,
            target_id: new_doc_id,
            document,
        } = replacements.get(position)?;
        let vectors = crate::mutation::vectors::document_vectors(
            context.keys.constraints.catalog,
            table,
            &document,
        )?;
        context.storage.insert_document(
            table,
            if remaps_primary_key {
                new_doc_id
            } else {
                old_doc_id
            },
            document,
            vectors,
            if remaps_primary_key {
                crate::mutation::publication::InsertedIdentity::Vacant
            } else {
                crate::mutation::publication::InsertedIdentity::Unknown
            },
        )?;
        if remaps_primary_key {
            context
                .state
                .advance_next_id(table, new_doc_id)
                .map_err(|error| {
                    crate::mutation::errors::identifier_storage_error(
                        "rewritten primary key",
                        &error,
                    )
                })?;
        }
    }
    Ok(())
}

/// The stored generated columns of `table`, whose values a rewrite of the table recomputes.
pub fn stored_generated_columns(
    constraints: ConstraintContext<'_>,
    table: &str,
) -> Result<Vec<String>, SQLError> {
    Ok(constraints
        .catalog
        .try_describe_table(table)
        .map_err(|error| crate::mutation::errors::dml_storage_error("table rewrite", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .into_iter()
        .filter(|column| {
            column.generated.as_ref().is_some_and(|generated| {
                generated.kind == uqa_sql::ast::GeneratedColumnKind::Stored
            })
        })
        .map(|column| column.name)
        .collect())
}
