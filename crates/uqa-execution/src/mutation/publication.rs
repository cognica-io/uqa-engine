//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed row-change publication carried from mutation application to transaction completion.

use std::collections::{BTreeMap, BTreeSet};

use crate::serializable::observe_row_write;
use uqa_core::DocId;
use uqa_sql::SQLError;

use super::prepared::{
    PreparedDeleteAction, PreparedDocumentDelete, PreparedDocumentInsert, PreparedDocumentRewrite,
    PreparedMutationAction,
};
use super::{identity::integer_primary_key_doc_id, vectors::document_vectors};
mod context;
pub use context::*;
mod identity;
pub use identity::InsertedIdentity;

const PREPARED_FTS_BATCH_DOCUMENTS: usize = 4_096;
type PreparedFtsDocuments = Vec<(DocId, BTreeMap<String, String>)>;
type PreparedFtsTables = BTreeMap<String, PreparedFtsDocuments>;

/// Whether writing a document changes its table's text index. A document known to be new that has no indexed text does not: no earlier version left postings to remove, and it adds none. A document that may replace an earlier version always does, because that version's postings go even when no text replaces them.
pub fn document_changes_text_index(
    known_new: bool,
    text_fields: &BTreeMap<String, String>,
) -> bool {
    !(known_new && text_fields.is_empty())
}

#[derive(Default)]
pub struct MutationPublicationBatch {
    fts_tables: PreparedFtsTables,
    fts_identities: BTreeMap<String, BTreeSet<DocId>>,
    fts_document_count: usize,
}

impl MutationPublicationBatch {
    fn push_fts(&mut self, table: String, doc_id: DocId, fields: BTreeMap<String, String>) {
        self.fts_identities
            .entry(table.clone())
            .or_default()
            .insert(doc_id);
        self.fts_tables
            .entry(table)
            .or_default()
            .push((doc_id, fields));
        self.fts_document_count += 1;
    }

    fn fts_is_full(&self) -> bool {
        self.fts_document_count >= PREPARED_FTS_BATCH_DOCUMENTS
    }

    fn flush_fts(&mut self, text: &dyn MutationTextIndex) -> Result<(), SQLError> {
        let tables = std::mem::take(&mut self.fts_tables);
        self.fts_identities.clear();
        self.fts_document_count = 0;
        for (table, documents) in tables {
            text.add_documents(&table, documents)?;
        }
        Ok(())
    }

    fn before_document(
        &mut self,
        text: &dyn MutationTextIndex,
        table: &str,
        doc_id: DocId,
    ) -> Result<(), SQLError> {
        if self
            .fts_identities
            .get(table)
            .is_some_and(|ids| ids.contains(&doc_id))
        {
            self.flush_fts(text)?;
        }
        Ok(())
    }
}

pub fn publish_prepared_mutation_action(
    context: PublicationContext<'_>,
    action: PreparedMutationAction,
    inserted: InsertedIdentity,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    match action {
        PreparedMutationAction::Insert(PreparedDocumentInsert {
            table,
            doc_id,
            document,
        }) => {
            batch.before_document(context.text, &table, doc_id)?;
            let text_fields = context.text.text_fields(&table, &document)?;
            let vectors = document_vectors(context.catalog, &table, &document)?;
            observe_row_write(context.observations, &table, doc_id)?;
            if !context.storage.can_defer_document_text(&table)? {
                batch.flush_fts(context.text)?;
                context
                    .storage
                    .insert_document(&table, doc_id, document, vectors, inserted)?;
                return context.deferrals.inserted(&table, doc_id);
            }
            context
                .storage
                .insert_document_deferred_text(&table, doc_id, document, vectors, inserted)?;
            context.deferrals.inserted(&table, doc_id)?;
            if document_changes_text_index(inserted.is_vacant(), &text_fields) {
                batch.push_fts(table, doc_id, text_fields);
                if batch.fts_is_full() {
                    batch.flush_fts(context.text)?;
                }
            }
        }
        PreparedMutationAction::Rewrite(mut rewrite) => {
            apply_document_rewrite(context, &mut rewrite, Some(batch))?;
        }
        PreparedMutationAction::Delete(mut delete) => {
            if !delete.actions.is_empty()
                || !context.storage.can_defer_document_text(&delete.table)?
            {
                batch.flush_fts(context.text)?;
                apply_validated_prepared_document_delete(context, &mut delete)?;
            } else {
                batch.before_document(context.text, &delete.table, delete.doc_id)?;
                observe_row_write(context.observations, &delete.table, delete.doc_id)?;
                context
                    .storage
                    .delete_document_deferred_text(&delete.table, delete.doc_id)?;
                batch.push_fts(delete.table, delete.doc_id, BTreeMap::new());
                if batch.fts_is_full() {
                    batch.flush_fts(context.text)?;
                }
            }
        }
    }
    Ok(())
}

pub fn finish_mutation_publication(
    context: PublicationContext<'_>,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    batch.flush_fts(context.text)
}

pub use crate::row_locks::publication::TransactionRowChange;

pub fn apply_validated_prepared_document_rewrite(
    context: PublicationContext<'_>,
    prepared: &mut PreparedDocumentRewrite,
) -> Result<DocId, SQLError> {
    apply_document_rewrite(context, prepared, None)
}

fn apply_document_rewrite(
    context: PublicationContext<'_>,
    prepared: &mut PreparedDocumentRewrite,
    mut batch: Option<&mut MutationPublicationBatch>,
) -> Result<DocId, SQLError> {
    if prepared.partition_move_delete.is_some()
        || prepared.destination.is_some()
        || !prepared.actions.is_empty()
        || (batch.is_some() && !context.storage.can_defer_document_text(&prepared.table)?)
    {
        if let Some(batch) = batch.take() {
            batch.flush_fts(context.text)?;
        }
    }
    if let Some(delete) = prepared.partition_move_delete.as_mut() {
        apply_validated_prepared_document_delete(context, delete)?;
        return Ok(prepared.doc_id);
    }
    if let Some((destination_table, destination_doc_id)) = prepared.destination.as_ref() {
        observe_row_write(context.observations, &prepared.table, prepared.doc_id)?;
        observe_row_write(context.observations, destination_table, *destination_doc_id)?;
        context
            .storage
            .delete_document(&prepared.table, prepared.doc_id)?;
        context.storage.insert_document(
            destination_table,
            *destination_doc_id,
            prepared.new_document.clone(),
            document_vectors(context.catalog, destination_table, &prepared.new_document)?,
            InsertedIdentity::Vacant,
        )?;
        context
            .identifiers
            .advance_next_id(destination_table, *destination_doc_id)
            .map_err(|err| {
                super::errors::identifier_storage_error("UPDATE partition movement", &err)
            })?;
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
                if let Some(batch) = batch {
                    batch.flush_fts(context.text)?;
                }
                observe_row_write(context.observations, &prepared.table, prepared.doc_id)?;
                observe_row_write(context.observations, &prepared.table, new_id)?;
                context
                    .storage
                    .delete_document(&prepared.table, prepared.doc_id)?;
                context.storage.insert_document(
                    &prepared.table,
                    new_id,
                    prepared.new_document.clone(),
                    document_vectors(context.catalog, &prepared.table, &prepared.new_document)?,
                    InsertedIdentity::Vacant,
                )?;
                context
                    .identifiers
                    .advance_next_id(&prepared.table, new_id)
                    .map_err(|err| {
                        super::errors::identifier_storage_error("UPDATE primary key", &err)
                    })?;
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
                rewrite_at_identity(context, prepared, batch)?;
                prepared.doc_id
            }
        };
    for action in &mut prepared.actions {
        apply_validated_prepared_document_rewrite(context, action)?;
    }
    Ok(rewritten_doc_id)
}

fn rewrite_at_identity(
    context: PublicationContext<'_>,
    prepared: &PreparedDocumentRewrite,
    batch: Option<&mut MutationPublicationBatch>,
) -> Result<(), SQLError> {
    observe_row_write(context.observations, &prepared.table, prepared.doc_id)?;
    if let Some(batch) = batch {
        batch.before_document(context.text, &prepared.table, prepared.doc_id)?;
        let fields = context
            .text
            .text_fields(&prepared.table, &prepared.new_document)?;
        context.storage.rewrite_document_deferred_text(
            &prepared.table,
            prepared.doc_id,
            prepared.new_document.clone(),
        )?;
        batch.push_fts(prepared.table.clone(), prepared.doc_id, fields);
        if batch.fts_is_full() {
            batch.flush_fts(context.text)?;
        }
    } else {
        context.storage.rewrite_document(
            &prepared.table,
            prepared.doc_id,
            prepared.new_document.clone(),
        )?;
    }
    context.deferrals.rewritten(
        &prepared.table,
        prepared.doc_id,
        Some(&prepared.old_document),
        &prepared.new_document,
    )
}

#[cfg(test)]
mod tests;

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
    observe_row_write(context.observations, &prepared.table, prepared.doc_id)?;
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
    inserted: InsertedIdentity,
    publication: &mut MutationPublicationBatch,
) -> Result<bool, SQLError> {
    match prepared {
        PreparedInsertConflict::Skip => Ok(false),
        PreparedInsertConflict::Updated(rewrite) => {
            publish_prepared_mutation_action(
                context,
                PreparedMutationAction::Rewrite(rewrite),
                InsertedIdentity::Unknown,
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
                inserted,
                publication,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Unresolved => Err(SQLError::Internal(
            "INSERT reached execution without a prepared document identity".into(),
        )),
    }
}
