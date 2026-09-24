//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, CommandOverlayDocument, DocId, Document, Engine, SQLError, Value};
use uqa_execution::query::document_changes::{DocumentChanges, DocumentSelection};
use uqa_execution::storage_errors::storage_error;
use uqa_storage::{DocumentMetadata, StoredDocument};

impl Engine {
    pub(super) fn command_overlay_table_name(&self, table: &str) -> Result<String, SQLError> {
        self.try_resolve_table_name(table)
            .map_err(|error| {
                SQLError::Internal(format!("resolve command-overlay table `{table}`: {error}"))
            })
            .map(|resolved| resolved.unwrap_or_else(|| table.to_string()))
    }

    pub(crate) fn command_mutation_overlay_active(&self) -> bool {
        if !self.session.command_mutation_overlays.lock().is_empty() {
            return true;
        }
        if let Some(overlay) = self.query_transaction_overlay.as_ref() {
            return !overlay.is_empty();
        }
        self.session
            .transactions
            .lock()
            .iter()
            .any(|frame| !frame.row_changes.is_empty())
    }

    pub(crate) fn stage_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<Document>,
    ) -> Result<(), SQLError> {
        self.stage_shared_command_document(table, doc_id, document.map(Arc::new))
    }

    pub(crate) fn stage_shared_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<Arc<Document>>,
    ) -> Result<(), SQLError> {
        let table = self.command_overlay_table_name(table)?;
        let control = self.query_retention_control()?;
        let document = document
            .map(|fields| -> Result<_, SQLError> {
                Ok((
                    fields,
                    DocumentMetadata::with_tuple_xmin(self.tuple_version_xid()?),
                ))
            })
            .transpose()?;
        let mut overlays = self.session.command_mutation_overlays.lock();
        let overlay = overlays.last_mut().ok_or_else(|| {
            SQLError::Internal("stage document without an active command overlay".into())
        })?;
        overlay.stage(&table, doc_id, document, &control)
    }

    pub(super) fn command_overlay_document(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<CommandOverlayDocument>, SQLError> {
        let table = self.command_overlay_table_name(table)?;
        Ok(self
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .rev()
            .find_map(|overlay| {
                overlay
                    .documents(&table)
                    .and_then(|documents| documents.get(&doc_id))
                    .map(|document| match document {
                        Some(document) => {
                            CommandOverlayDocument::Present(StoredDocument::with_metadata(
                                document.fields.as_ref().clone(),
                                document.metadata,
                            ))
                        }
                        None => CommandOverlayDocument::Deleted,
                    })
            }))
    }

    pub(super) fn command_overlay_exact_match(
        &self,
        table: &str,
        fields: &[String],
        values: &[Value],
        presence: uqa_execution::query::exact_lookup::FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        let table = self.command_overlay_table_name(table)?;
        let control = self.query_retention_control()?;
        uqa_execution::mutation::overlay::CommandMutationOverlay::find_match(
            &mut self.session.command_mutation_overlays.lock(),
            &table,
            fields,
            values,
            presence,
            &control,
        )
    }

    pub(crate) fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<DocumentChanges>, SQLError> {
        let canonical = self.command_overlay_table_name(table)?;
        let mut changes = self
            .fixed_transaction_row_changes(&canonical)?
            .unwrap_or_default();
        let overlays = self.session.command_mutation_overlays.lock();
        if overlays.is_empty() && !changes.has_changes() {
            return Ok(None);
        }
        let control = self.query_retention_control()?;
        for overlay in overlays.iter() {
            if let Some(documents) = overlay.documents(&canonical) {
                let additions = DocumentChanges::from_retained(
                    documents.iter().map(|(id, document)| {
                        (
                            *id,
                            document
                                .as_ref()
                                .map(|document| (document.fields.clone(), document.metadata)),
                        )
                    }),
                    &control,
                )
                .map_err(|error| storage_error("capture command selection", &error))?;
                changes
                    .extend(additions, &control)
                    .map_err(|error| storage_error("merge command selection", &error))?;
            }
        }
        Ok(Some(changes))
    }

    pub(crate) fn fixed_transaction_row_changes(
        &self,
        canonical_table: &str,
    ) -> Result<Option<DocumentChanges>, SQLError> {
        let mut changes = self
            .query_transaction_overlay
            .as_ref()
            .and_then(|overlay| overlay.get(canonical_table).cloned())
            .unwrap_or_default();
        if self.query_transaction_overlay.is_some() && self.query_transaction_origin.is_none() {
            return Ok(changes.has_changes().then_some(changes));
        }
        let relation = crate::RelationIdentity::from_legacy_name(canonical_table)
            .map_err(SQLError::Internal)?;
        let query_table = self
            .query_table_snapshots
            .as_ref()
            .and_then(|snapshots| snapshots.get(&relation))
            .cloned()
            .or_else(|| self.storage.tables.read().get(&relation).cloned());
        let generation = query_table.map(|table| table.storage_generation());
        let Some(generation) = generation else {
            return Ok(changes.has_changes().then_some(changes));
        };
        let control = self.query_retention_control()?;
        let desired = {
            let stack = self.session.transactions.lock();
            if self.query_transaction_overlay.is_none()
                && stack
                    .first()
                    .is_none_or(|frame| frame.fixed_snapshot.is_none())
            {
                return Ok(None);
            }
            let mut desired = DocumentSelection::new(&control);
            for change in stack.iter().flat_map(|frame| frame.row_changes.iter()) {
                if self
                    .query_transaction_origin
                    .is_some_and(|origin| change.query_origin != Some(origin))
                {
                    continue;
                }
                if change.source_generation == generation {
                    desired
                        .insert(
                            change.pending.key.doc_id,
                            !matches!(
                                change.pending.kind,
                                crate::row_locks::PendingRowChangeKind::Delete
                                    | crate::row_locks::PendingRowChangeKind::Rewrite(_)
                            ),
                            &control,
                        )
                        .map_err(|error| storage_error("select private query rows", &error))?;
                }
                if let crate::row_locks::PendingRowChangeKind::Rewrite(successor) =
                    change.pending.kind
                {
                    if change.successor_generation == Some(generation) {
                        desired
                            .insert(successor.doc_id, true, &control)
                            .map_err(|error| storage_error("select private query rows", &error))?;
                    }
                }
            }
            desired
        };
        if desired.is_empty() {
            return Ok(changes.has_changes().then_some(changes));
        }
        let live = self
            .storage
            .tables
            .read()
            .values()
            .find(|table| table.storage_generation() == generation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "transaction row changes refer to unavailable relation generation for `{canonical_table}`"
                ))
            })?;
        changes
            .extend(
                self.capture_query_document_changes(&live, desired)?,
                &control,
            )
            .map_err(|error| storage_error("merge private query rows", &error))?;
        Ok(Some(changes))
    }
}
