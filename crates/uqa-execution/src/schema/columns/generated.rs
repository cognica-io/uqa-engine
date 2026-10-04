//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite a table's rows as `ATRewriteTable` does: recompute generated columns, check the new rows, remap physical primary keys, and validate the foreign keys involved.
use crate::mutation::{
    assignment::MutationAssignmentContext, constraints::context::ConstraintContext,
    publication::MutationStorage,
};
use crate::schema::keys::KeyValidationContext;
use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::{document_store::Document, StorageBackendResult};
pub trait GeneratedRewriteState {
    fn advance_next_id(&self, table: &str, id: DocId) -> StorageBackendResult<()>;
}
pub struct GeneratedRewriteContext<'a, S: Clone + 'static> {
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
    let doc_ids = context.keys.constraints.reads.live_table_doc_ids(table)?;
    let mut rows = Vec::with_capacity(doc_ids.len());
    for doc_id in &doc_ids {
        let Some(mut document) = context
            .keys
            .constraints
            .reads
            .get_document(table, *doc_id)?
        else {
            continue;
        };
        crate::mutation::assignment::refresh_stored_generated_columns(
            context.assignment,
            table,
            &mut document,
        )?;
        rows.push((*doc_id, document));
    }
    let changed = stored_generated_columns(context.keys.constraints, table)?;
    rewrite_table_rows(context, table, rows, rewrite_physical_rows, &changed)
}

/// Replace the rows of `table` with the rows a rewrite produced, as `ATRewriteTable` does. Each new row is checked against the table's validated NOT NULL and CHECK constraints, then the key constraints are checked across all of them, as rebuilding their indexes does; when `write`, the rows replace the old ones, a row whose integer primary key changed moving to the identity that key names. The foreign keys that involve a `changed` column are validated against the result.
pub fn rewrite_table_rows<S: Clone + 'static>(
    context: &GeneratedRewriteContext<'_, S>,
    table: &str,
    rows: Vec<(DocId, Document)>,
    write: bool,
    changed: &[String],
) -> Result<(), SQLError> {
    for (_, document) in &rows {
        crate::mutation::constraints::validate_rewritten_row(
            context.keys.constraints,
            table,
            document,
        )?;
    }
    super::super::keys::validate_key_constraint_rows(&context.keys, table, &rows)?;
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
    rows: Vec<(DocId, Document)>,
) -> Result<(), SQLError> {
    let mut replacements = Vec::with_capacity(rows.len());
    let mut remaps_primary_key = false;
    for (old_doc_id, document) in rows {
        let new_doc_id = crate::mutation::identity::key_relocation(
            context.keys.constraints.catalog,
            context.identifiers,
            table,
            old_doc_id,
            &document,
        )?
        .unwrap_or(old_doc_id);
        remaps_primary_key |= new_doc_id != old_doc_id;
        replacements.push((old_doc_id, new_doc_id, document));
    }
    if remaps_primary_key {
        for (old_doc_id, _, _) in &replacements {
            context.storage.delete_document(table, *old_doc_id)?;
        }
    }
    for (old_doc_id, new_doc_id, document) in replacements {
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
