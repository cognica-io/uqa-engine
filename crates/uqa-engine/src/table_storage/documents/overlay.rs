//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, CommandOverlayDocument, DocId, Document, Engine, SQLError, Value};
use uqa_execution::mutation::overlay::CommandMutationOverlay;
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

    /// Whether a read of `table` merges changes that its storage view does not show, as [`Self::command_overlay_changes`] would return them: documents the running command staged for the table, changes a portal's transaction overlay holds for it, or rows of it that this transaction changed while its reads see a fixed snapshot. A read of any other table sees exactly its storage view.
    pub(crate) fn command_overlay_holds(&self, table: &str) -> Result<bool, SQLError> {
        // A statement outside a transaction, or before the transaction's first write, has nothing staged, held or changed, and needs no name resolution to know it.
        let staging = !self.session.command_mutation_overlays.lock().is_empty();
        let holding = self
            .query_transaction_overlay
            .as_ref()
            .is_some_and(|overlay| !overlay.is_empty());
        if !staging
            && !holding
            && self
                .session
                .transactions
                .lock()
                .iter()
                .all(|frame| frame.row_changes.is_empty())
        {
            return Ok(false);
        }
        let canonical = self.command_overlay_table_name(table)?;
        let staged = self
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .any(|overlay| overlay.holds(&canonical))
            || self
                .query_transaction_overlay
                .as_ref()
                .and_then(|overlay| overlay.get(&canonical))
                .is_some_and(DocumentChanges::has_changes);
        if staged
            || (self.query_transaction_overlay.is_some() && self.query_transaction_origin.is_none())
        {
            return Ok(staged);
        }
        let Some(generation) = self.query_relation_generation(&canonical)? else {
            return Ok(false);
        };
        if self.reads_identity_index() {
            return Ok(self.transaction_reads_are_fixed()
                && self.fixed_identities_view(&generation)?.is_some());
        }
        let mut changed = false;
        self.visit_fixed_transaction_rows(generation, &mut |_, _| {
            changed = true;
            Ok(false)
        })?;
        Ok(changed)
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
        if self.session.command_mutation_overlays.lock().is_empty() {
            return Ok(None);
        }
        let table = self.command_overlay_table_name(table)?;
        let control = self.query_retention_control()?;
        let overlays = self.session.command_mutation_overlays.lock();
        Ok(
            CommandMutationOverlay::row(&overlays, &table, doc_id, &control)?.map(|document| {
                match document {
                    Some(document) => {
                        CommandOverlayDocument::Present(StoredDocument::with_metadata(
                            document.fields.into_document(),
                            document.metadata,
                        ))
                    }
                    None => CommandOverlayDocument::Deleted,
                }
            }),
        )
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
        CommandMutationOverlay::find_match(
            &mut self.session.command_mutation_overlays.lock(),
            &table,
            fields,
            values,
            presence,
            &control,
        )
    }

    /// The visible rows the running commands staged for `table` whose `fields` hold `values`.
    pub(crate) fn command_overlay_matches(
        &self,
        table: &str,
        fields: &[String],
        values: &[Value],
    ) -> Result<Vec<DocId>, SQLError> {
        if self.session.command_mutation_overlays.lock().is_empty() {
            return Ok(Vec::new());
        }
        let table = self.command_overlay_table_name(table)?;
        let control = self.query_retention_control()?;
        let mut overlays = self.session.command_mutation_overlays.lock();
        let (matches, _memory) =
            CommandMutationOverlay::matches(&mut overlays, &table, fields, values, &control)?
                .into_parts();
        Ok(matches)
    }

    /// At least the number of rows the running commands staged for `table`, without reading them.
    pub(crate) fn command_overlay_row_bound(&self, table: &str) -> Result<u64, SQLError> {
        if self.session.command_mutation_overlays.lock().is_empty() {
            return Ok(0);
        }
        let table = self.command_overlay_table_name(table)?;
        Ok(CommandMutationOverlay::staged_row_bound(
            &self.session.command_mutation_overlays.lock(),
            &table,
        ))
    }

    pub(crate) fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<DocumentChanges>, SQLError> {
        let canonical = self.command_overlay_table_name(table)?;
        let changes = self
            .fixed_transaction_row_changes(&canonical)?
            .unwrap_or_default();
        let overlays = self.session.command_mutation_overlays.lock();
        if overlays.is_empty() && !changes.has_changes() {
            return Ok(None);
        }
        let control = self.query_retention_control()?;
        CommandMutationOverlay::changes(&overlays, &canonical, changes, &control).map(Some)
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
        let Some(generation) = self.query_relation_generation(canonical_table)? else {
            return Ok(changes.has_changes().then_some(changes));
        };
        if self.reads_identity_index() {
            return self.indexed_transaction_row_changes(canonical_table, &generation);
        }
        let control = self.query_retention_control()?;
        let mut desired = DocumentSelection::new(&control);
        let fixed = self.visit_fixed_transaction_rows(generation, &mut |doc_id, live| {
            desired
                .insert(doc_id, live, &control)
                .map_err(|error| storage_error("select private query rows", &error))?;
            Ok(true)
        })?;
        if !fixed {
            return Ok(None);
        }
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

    /// Whether this read takes the rows its transaction changed from the transaction's index of them: a session read, rather than a portal's, of a relation whose provider keeps immutable views of the transaction's rows.
    fn reads_identity_index(&self) -> bool {
        self.query_transaction_overlay.is_none()
            && self.query_transaction_origin.is_none()
            && (self.storage.backend.is_none() || self.versioned_backend_transactions())
    }

    /// Whether this transaction's reads see a fixed snapshot, which the rows it changed must be merged into; reads that see the transaction's changes in storage merge nothing.
    fn transaction_reads_are_fixed(&self) -> bool {
        self.query_transaction_overlay.is_some()
            || self
                .session
                .transactions
                .lock()
                .first()
                .is_some_and(|frame| frame.fixed_snapshot.is_some())
    }

    /// The rows this transaction changed in relation generation `generation`, as a view of its index above nothing else, read from the relation's live rows.
    fn indexed_transaction_row_changes(
        &self,
        canonical_table: &str,
        generation: &[u8; 16],
    ) -> Result<Option<DocumentChanges>, SQLError> {
        if !self.transaction_reads_are_fixed() {
            return Ok(None);
        }
        let Some(view) = self.fixed_identities_view(generation)? else {
            return Ok(None);
        };
        let live = self
            .storage
            .tables
            .read()
            .values()
            .find(|table| table.storage_generation() == *generation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "transaction row changes refer to unavailable relation generation for `{canonical_table}`"
                ))
            })?;
        let control = self.query_retention_control()?;
        let source = live
            .document_store
            .read()
            .snapshot()
            .map_err(|error| storage_error("capture private document source", &error))?;
        let columns = live.columns.read();
        let vectors = live.vector_indexes.read();
        let changes = DocumentChanges::default()
            .with_identities(view, source, &columns, &*vectors, &control)
            .map_err(|error| storage_error("capture private document changes", &error))?;
        Ok(Some(changes))
    }

    /// The storage generation of the relation a read of `canonical_table` sees, or `None` when no such relation is loaded.
    fn query_relation_generation(
        &self,
        canonical_table: &str,
    ) -> Result<Option<[u8; 16]>, SQLError> {
        let relation = crate::RelationIdentity::from_legacy_name(canonical_table)
            .map_err(SQLError::Internal)?;
        let query_table = self
            .query_table_snapshots
            .as_ref()
            .and_then(|snapshots| snapshots.get(&relation))
            .cloned()
            .or_else(|| self.storage.tables.read().get(&relation).cloned());
        Ok(query_table.map(|table| table.storage_generation()))
    }

    /// Visit the rows of relation generation `generation` that this transaction changed and a read of a fixed snapshot merges, each with whether it is live afterwards, while `visit` returns true. Returns false without visiting a row when the transaction's reads see its changes in storage.
    fn visit_fixed_transaction_rows(
        &self,
        generation: [u8; 16],
        visit: &mut dyn FnMut(DocId, bool) -> Result<bool, SQLError>,
    ) -> Result<bool, SQLError> {
        let stack = self.session.transactions.lock();
        if self.query_transaction_overlay.is_none()
            && stack
                .first()
                .is_none_or(|frame| frame.fixed_snapshot.is_none())
        {
            return Ok(false);
        }
        for change in stack.iter().flat_map(|frame| frame.row_changes.iter()) {
            if self
                .query_transaction_origin
                .is_some_and(|origin| change.query_origin != Some(origin))
            {
                continue;
            }
            if change.source_generation == generation
                && !visit(
                    change.pending.key.doc_id,
                    !matches!(
                        change.pending.kind,
                        crate::row_locks::PendingRowChangeKind::Delete
                            | crate::row_locks::PendingRowChangeKind::Rewrite(_)
                    ),
                )?
            {
                break;
            }
            if let crate::row_locks::PendingRowChangeKind::Rewrite(successor) = change.pending.kind
            {
                if change.successor_generation == Some(generation)
                    && !visit(successor.doc_id, true)?
                {
                    break;
                }
            }
        }
        Ok(true)
    }
}
