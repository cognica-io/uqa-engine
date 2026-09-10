//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recompute generated rows, remap physical primary keys, and revalidate stored relations.
use crate::mutation::{
    assignment::MutationAssignmentContext, constraints::context::ConstraintContext,
    publication::MutationStorage,
};
use crate::schema::keys::KeyValidationContext;
use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::StorageBackendResult;
pub trait GeneratedRewriteState {
    fn table_names(&self) -> StorageBackendResult<Vec<String>>;
    fn advance_next_id(&self, table: &str, id: DocId) -> StorageBackendResult<()>;
}
pub struct GeneratedRewriteContext<'a, S: Clone + 'static> {
    pub keys: KeyValidationContext<'a>,
    pub assignment: MutationAssignmentContext<'a, S>,
    pub storage: &'a dyn MutationStorage,
    pub state: &'a dyn GeneratedRewriteState,
}
fn ddl_storage_error(action: &str, error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
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
    super::super::keys::validate_key_constraint_rows(&context.keys, table, &rows)?;
    if rewrite_physical_rows {
        let mut replacements = Vec::with_capacity(rows.len());
        let mut remaps_primary_key = false;
        for (old_doc_id, document) in rows {
            let new_doc_id = crate::mutation::identity::integer_primary_key_doc_id(
                context.keys.constraints.catalog,
                table,
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
                remaps_primary_key,
            )?;
            if remaps_primary_key {
                context
                    .state
                    .advance_next_id(table, new_doc_id)
                    .map_err(|error| ddl_storage_error("generated primary key rewrite", error))?;
            }
        }
    }
    validate_all_table_rows(context.state, context.keys.constraints)
}

pub fn validate_all_table_rows(
    catalog: &dyn GeneratedRewriteState,
    constraints: ConstraintContext<'_>,
) -> Result<(), SQLError> {
    for table in catalog
        .table_names()
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        for doc_id in constraints.reads.live_table_doc_ids(&table)? {
            let Some(document) = constraints.reads.get_document(&table, doc_id)? else {
                continue;
            };
            crate::mutation::constraints::validate_document_constraints(
                constraints,
                &table,
                &document,
                &[],
                Some(doc_id),
            )?;
        }
    }
    Ok(())
}
