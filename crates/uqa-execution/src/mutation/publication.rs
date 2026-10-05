//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed row-change publication carried from mutation application to transaction completion.

use std::collections::{BTreeMap, BTreeSet};

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::serializable::observe_row_write;
use uqa_core::DocId;
use uqa_sql::SQLError;

use super::prepared::{
    PreparedDocumentDelete, PreparedDocumentInsert, PreparedDocumentRewrite, PreparedMutationAction,
};
use super::vectors::document_vectors;
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
    published: Vec<super::overlay::CommandMutationOverlay>,
    fts_tables: PreparedFtsTables,
    fts_identities: BTreeMap<String, BTreeSet<DocId>>,
    fts_document_count: usize,
    /// The rows the batch's actions wrote, kept for a statement whose other commands treat them as rows the statement already modified.
    written: Option<Vec<PhysicalDocumentIdentity>>,
}

impl MutationPublicationBatch {
    pub fn with_published(
        mut self,
        published: Option<super::overlay::CommandMutationOverlay>,
    ) -> Self {
        self.published.extend(published);
        self
    }

    fn was_published(
        &self,
        context: PublicationContext<'_>,
        table: &str,
        doc_id: DocId,
        deleted_supersedes: bool,
    ) -> Result<bool, SQLError> {
        for overlay in self.published.iter().rev() {
            if overlay.was_published(
                table,
                doc_id,
                deleted_supersedes,
                context.observations.serializable_cancellation(),
            )? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A batch that keeps the identity of every row its actions write when `record` holds.
    pub fn recording_writes(record: bool) -> Self {
        Self {
            written: record.then(Vec::new),
            ..Self::default()
        }
    }

    /// The rows the batch's actions wrote, when it keeps them.
    pub fn take_written(&mut self) -> Vec<PhysicalDocumentIdentity> {
        self.written.take().unwrap_or_default()
    }

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
    if let Some(written) = batch.written.as_mut() {
        written.extend(action.written_rows());
    }
    match action {
        PreparedMutationAction::Insert(PreparedDocumentInsert {
            table,
            doc_id,
            document,
        }) => {
            if batch.was_published(context, &table, doc_id, true)? {
                return context.deferrals.inserted(&table, doc_id);
            }
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
        PreparedMutationAction::Rewrite(rewrite) => {
            apply_document_rewrite(context, &rewrite, Some(batch))?;
        }
        PreparedMutationAction::Delete(delete) => {
            if batch.was_published(context, &delete.table, delete.doc_id, false)? {
                return Ok(());
            }
            if context.storage.can_defer_document_text(&delete.table)? {
                batch.before_document(context.text, &delete.table, delete.doc_id)?;
                observe_row_write(context.observations, &delete.table, delete.doc_id)?;
                context
                    .storage
                    .delete_document_deferred_text(&delete.table, delete.doc_id)?;
                batch.push_fts(delete.table, delete.doc_id, BTreeMap::new());
                if batch.fts_is_full() {
                    batch.flush_fts(context.text)?;
                }
            } else {
                batch.flush_fts(context.text)?;
                apply_validated_prepared_document_delete(context, &delete)?;
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
    prepared: &PreparedDocumentRewrite,
) -> Result<DocId, SQLError> {
    apply_document_rewrite(context, prepared, None)
}

fn apply_document_rewrite(
    context: PublicationContext<'_>,
    prepared: &PreparedDocumentRewrite,
    mut batch: Option<&mut MutationPublicationBatch>,
) -> Result<DocId, SQLError> {
    let (destination_table, destination_doc_id) = prepared.destination.as_ref().map_or(
        (
            prepared.table.as_str(),
            prepared.relocation.unwrap_or(prepared.doc_id),
        ),
        |(table, id)| (table.as_str(), *id),
    );
    let destination_published = batch.as_deref().map_or(Ok(false), |batch| {
        batch.was_published(context, destination_table, destination_doc_id, true)
    })?;
    let source_replaced = batch.as_deref().map_or(Ok(false), |batch| {
        batch.was_published(context, &prepared.table, prepared.doc_id, false)
    })?;
    let moves_row = prepared.destination.is_some() || destination_doc_id != prepared.doc_id;
    if prepared.partition_move_delete.is_some()
        || moves_row
        || (batch.is_some() && !context.storage.can_defer_document_text(&prepared.table)?)
    {
        if let Some(batch) = batch.take() {
            batch.flush_fts(context.text)?;
        }
    }
    if let Some(delete) = prepared.partition_move_delete.as_deref() {
        if !source_replaced {
            apply_validated_prepared_document_delete(context, delete)?;
        }
        return Ok(prepared.doc_id);
    }
    if moves_row {
        return rewrite_to_identity(
            context,
            prepared,
            (destination_table, destination_doc_id),
            source_replaced,
            destination_published,
        );
    }
    if destination_published {
        context.deferrals.rewritten(
            &prepared.table,
            prepared.doc_id,
            Some(&prepared.old_document),
            &prepared.new_document,
        )?;
    } else {
        rewrite_at_identity(context, prepared, batch)?;
    }
    Ok(prepared.doc_id)
}

/// Publish the remaining parts of a key or partition move without deleting a nested reinsertion at its source or replacing a nested write at its destination.
fn rewrite_to_identity(
    context: PublicationContext<'_>,
    prepared: &PreparedDocumentRewrite,
    (destination_table, destination_doc_id): (&str, DocId),
    source_replaced: bool,
    destination_published: bool,
) -> Result<DocId, SQLError> {
    if !source_replaced {
        observe_row_write(context.observations, &prepared.table, prepared.doc_id)?;
        context
            .storage
            .delete_document(&prepared.table, prepared.doc_id)?;
    }
    if !destination_published {
        observe_row_write(context.observations, destination_table, destination_doc_id)?;
        context.storage.insert_document(
            destination_table,
            destination_doc_id,
            prepared.new_document.clone(),
            document_vectors(context.catalog, destination_table, &prepared.new_document)?,
            InsertedIdentity::Vacant,
        )?;
    }
    let partition_move = prepared.destination.is_some();
    context
        .identifiers
        .advance_next_id(destination_table, destination_doc_id)
        .map_err(|err| {
            super::errors::identifier_storage_error(
                if partition_move {
                    "UPDATE partition movement"
                } else {
                    "UPDATE primary key"
                },
                &err,
            )
        })?;
    context.history.note_rewrite(
        &prepared.table,
        prepared.doc_id,
        destination_table,
        destination_doc_id,
    )?;
    context.deferrals.rewritten(
        destination_table,
        destination_doc_id,
        (!partition_move).then_some(&prepared.old_document),
        &prepared.new_document,
    )?;
    Ok(destination_doc_id)
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
    prepared: &PreparedDocumentDelete,
) -> Result<(), SQLError> {
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
