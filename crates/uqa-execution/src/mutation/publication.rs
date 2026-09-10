//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed row-change publication carried from mutation application to transaction completion.

use std::collections::BTreeMap;

use uqa_core::DocId;
use uqa_sql::SQLError;

use super::prepared::{
    PreparedDeleteAction, PreparedDocumentDelete, PreparedDocumentInsert, PreparedDocumentRewrite,
    PreparedMutationAction,
};
use super::{
    errors::dml_storage_error, identity::integer_primary_key_doc_id, vectors::document_vectors,
};
mod context;
pub use context::*;

const PREPARED_FTS_BATCH_DOCUMENTS: usize = 4_096;
type PreparedFtsDocuments = Vec<(DocId, BTreeMap<String, String>)>;
type PreparedFtsTables = BTreeMap<String, PreparedFtsDocuments>;

#[derive(Default)]
pub struct MutationPublicationBatch {
    fts_tables: PreparedFtsTables,
    fts_document_count: usize,
}

impl MutationPublicationBatch {
    fn push_fts(&mut self, table: String, doc_id: DocId, fields: BTreeMap<String, String>) {
        self.fts_tables
            .entry(table)
            .or_default()
            .push((doc_id, fields));
        self.fts_document_count += 1;
    }

    fn fts_is_full(&self) -> bool {
        self.fts_document_count >= PREPARED_FTS_BATCH_DOCUMENTS
    }

    fn flush_fts(&mut self, context: PublicationContext<'_>) -> Result<(), SQLError> {
        let tables = std::mem::take(&mut self.fts_tables);
        self.fts_document_count = 0;
        for (table, documents) in tables {
            context.text.add_documents(&table, documents)?;
        }
        Ok(())
    }
}

pub fn publish_prepared_mutation_action(
    context: PublicationContext<'_>,
    action: PreparedMutationAction,
    insert_known_new: bool,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    match action {
        PreparedMutationAction::Insert(PreparedDocumentInsert {
            table,
            doc_id,
            document,
        }) => {
            let text_fields = context.text.text_fields(&table, &document)?;
            let vectors = document_vectors(context.catalog, &table, &document)?;
            context.storage.insert_document_deferred_text(
                &table,
                doc_id,
                document,
                vectors,
                insert_known_new,
            )?;
            context.deferrals.inserted(&table, doc_id)?;
            batch.push_fts(table, doc_id, text_fields);
            if batch.fts_is_full() {
                batch.flush_fts(context)?;
            }
        }
        PreparedMutationAction::Rewrite(mut rewrite) => {
            batch.flush_fts(context)?;
            apply_validated_prepared_document_rewrite(context, &mut rewrite)?;
        }
        PreparedMutationAction::Delete(mut delete) => {
            batch.flush_fts(context)?;
            apply_validated_prepared_document_delete(context, &mut delete)?;
        }
    }
    Ok(())
}

pub fn finish_mutation_publication(
    context: PublicationContext<'_>,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    batch.flush_fts(context)
}

pub use crate::row_locks::publication::TransactionRowChange;

pub fn apply_validated_prepared_document_rewrite(
    context: PublicationContext<'_>,
    prepared: &mut PreparedDocumentRewrite,
) -> Result<DocId, SQLError> {
    if let Some(delete) = prepared.partition_move_delete.as_mut() {
        apply_validated_prepared_document_delete(context, delete)?;
        return Ok(prepared.doc_id);
    }
    if let Some((destination_table, destination_doc_id)) = prepared.destination.as_ref() {
        context
            .storage
            .delete_document(&prepared.table, prepared.doc_id)?;
        context.storage.insert_document(
            destination_table,
            *destination_doc_id,
            prepared.new_document.clone(),
            document_vectors(context.catalog, destination_table, &prepared.new_document)?,
            true,
        )?;
        context
            .identifiers
            .advance_next_id(destination_table, *destination_doc_id)
            .map_err(|err| dml_storage_error("UPDATE partition movement", err))?;
        context.history.note_rewrite(
            &prepared.table,
            prepared.doc_id,
            destination_table,
            *destination_doc_id,
        )?;
        context.deferrals.rewritten(
            destination_table,
            *destination_doc_id,
            None,
            &prepared.new_document,
        )?;
        for action in &mut prepared.actions {
            apply_validated_prepared_document_rewrite(context, action)?;
        }
        return Ok(*destination_doc_id);
    }
    let rewritten_doc_id =
        match integer_primary_key_doc_id(context.catalog, &prepared.table, &prepared.new_document)?
        {
            // An integer primary key names the row's doc_id slot; keep that invariant when the key itself changes, or value -> doc_id lookups (the unique fast path and FOREIGN KEY validation) read the stale slot and miss the row.
            Some(new_id) if new_id != prepared.doc_id => {
                context
                    .storage
                    .delete_document(&prepared.table, prepared.doc_id)?;
                context.storage.insert_document(
                    &prepared.table,
                    new_id,
                    prepared.new_document.clone(),
                    document_vectors(context.catalog, &prepared.table, &prepared.new_document)?,
                    true,
                )?;
                context
                    .identifiers
                    .advance_next_id(&prepared.table, new_id)
                    .map_err(|err| dml_storage_error("UPDATE primary key", err))?;
                context.history.note_rewrite(
                    &prepared.table,
                    prepared.doc_id,
                    &prepared.table,
                    new_id,
                )?;
                context.deferrals.rewritten(
                    &prepared.table,
                    new_id,
                    Some(&prepared.old_document),
                    &prepared.new_document,
                )?;
                new_id
            }
            _ => {
                context.storage.rewrite_document(
                    &prepared.table,
                    prepared.doc_id,
                    prepared.new_document.clone(),
                )?;
                context.deferrals.rewritten(
                    &prepared.table,
                    prepared.doc_id,
                    Some(&prepared.old_document),
                    &prepared.new_document,
                )?;
                prepared.doc_id
            }
        };
    for action in &mut prepared.actions {
        apply_validated_prepared_document_rewrite(context, action)?;
    }
    Ok(rewritten_doc_id)
}

pub fn apply_validated_prepared_document_delete(
    context: PublicationContext<'_>,
    prepared: &mut PreparedDocumentDelete,
) -> Result<(), SQLError> {
    for action in &mut prepared.actions {
        match action {
            PreparedDeleteAction::Delete(delete) => {
                apply_validated_prepared_document_delete(context, delete)?;
            }
            PreparedDeleteAction::Rewrite(rewrite) => {
                apply_validated_prepared_document_rewrite(context, rewrite)?;
            }
        }
    }
    context
        .storage
        .delete_document(&prepared.table, prepared.doc_id)
}

use super::prepared::PreparedInsertConflict;
use uqa_storage::document_store::Document;
pub fn apply_validated_prepared_insert(
    context: PublicationContext<'_>,
    table: &str,
    document: Document,
    prepared: PreparedInsertConflict,
    known_new: bool,
    publication: &mut MutationPublicationBatch,
) -> Result<bool, SQLError> {
    match prepared {
        PreparedInsertConflict::Skip => Ok(false),
        PreparedInsertConflict::Updated(rewrite) => {
            publish_prepared_mutation_action(
                context,
                PreparedMutationAction::Rewrite(rewrite),
                false,
                publication,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Insert { doc_id, .. } => {
            publish_prepared_mutation_action(
                context,
                PreparedMutationAction::Insert(PreparedDocumentInsert {
                    table: table.to_string(),
                    doc_id,
                    document,
                }),
                known_new,
                publication,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Unresolved => Err(SQLError::Internal(
            "INSERT reached execution without a prepared document identity".into(),
        )),
    }
}
