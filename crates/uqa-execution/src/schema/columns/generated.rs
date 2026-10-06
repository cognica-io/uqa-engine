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
/// Publish an already assigned backfill field through the prepared storage path. A schema rewrite must preserve all unrelated stored generated values instead of invoking ordinary DML generation.
pub fn update_rewritten_fields<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    id: DocId,
    values: std::collections::BTreeMap<String, uqa_core::Value>,
    vectors: crate::mutation::publication::DocumentVectors,
) -> Result<bool, SQLError> {
    context.cancellation.check()?;
    let Some(mut document) = context.keys.constraints.reads.get_document(table, id)? else {
        return Ok(false);
    };
    document.extend(values);
    let mut replacement_vectors = crate::mutation::vectors::document_vectors(
        context.keys.constraints.catalog,
        table,
        &document,
    )?;
    replacement_vectors.extend(vectors);
    context.storage.insert_document(
        table,
        id,
        document,
        replacement_vectors,
        crate::mutation::publication::InsertedIdentity::Unknown,
    )?;
    Ok(true)
}

/// Recompute the requested stored generated columns of every row of `table` and, when `rewrite_physical_rows`, rewrite the table with them, as adding a stored generated column or changing its expression does.
pub fn validate_and_rewrite_generated_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    rewrite_physical_rows: bool,
    changed: &[String],
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
        crate::mutation::assignment::refresh_selected_stored_generated_columns(
            context.assignment,
            table,
            &mut document,
            Some(changed),
        )?;
        rows.push(doc_id, document)?;
    }
    drop(original);
    rewrite_table_rows(context, table, rows, rewrite_physical_rows, changed)
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
    publish_validated_rows(context, table, rows, write)?;
    crate::schema::validation::validate_rewritten_foreign_keys(
        context.keys.constraints,
        table,
        changed,
    )
}

/// Publish rows whose row-local checks have already run in conversion order, then verify the cross-row keys.
pub(in crate::schema) fn publish_validated_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    mut rows: RewriteRows,
    write: bool,
) -> Result<(), SQLError> {
    super::super::keys::validate_key_constraint_rows(&context.keys, table, &mut rows)?;
    if write {
        publish_rewritten_rows(context, table, rows)?;
        super::super::keys::validate_temporal_key_rows(&context.keys, table)?;
    }
    Ok(())
}

fn publish_rewritten_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    mut rows: RewriteRows,
) -> Result<(), SQLError> {
    let mut replacements = RewriteRows::new(rows.memory());
    for position in 0..rows.len() {
        context.cancellation.check()?;
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
        replacements.push_replacement(old_doc_id, new_doc_id, document)?;
    }
    drop(rows);
    // A rewrite replaces the heap captured at its start. Self-modifying callbacks cannot leave additional old-heap rows beside that replacement.
    replacements.spill()?;
    remove_replaced_rows(context, table, replacements.memory())?;
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
            new_doc_id,
            document,
            vectors,
            crate::mutation::publication::InsertedIdentity::Vacant,
        )?;
        if new_doc_id != old_doc_id {
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

fn remove_replaced_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    memory: &uqa_core::memory::MemoryBudget,
) -> Result<(), SQLError> {
    let control = uqa_storage::read_control::StorageReadControl::new(memory, context.cancellation);
    let limit = (memory.available() / (4 * std::mem::size_of::<DocId>()))
        .clamp(1, crate::DEFAULT_BATCH_SIZE);
    let mut after = None;
    loop {
        context.cancellation.check()?;
        let ids = context
            .keys
            .constraints
            .reads
            .live_table_doc_id_page(table, after, limit, &control)?;
        let Some(last) = ids.last().copied() else {
            return Ok(());
        };
        after = Some(last);
        for id in ids.iter().copied() {
            context.cancellation.check()?;
            context.storage.delete_document(table, id)?;
        }
    }
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
