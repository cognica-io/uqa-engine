//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    command_exact_document_key, command_exact_lookup_parts, document_store_read_error, Arc,
    BTreeMap, CommandOverlayDocument, DocId, Document, Engine, SQLError, Value,
};
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
        let document = document
            .map(|fields| -> Result<_, SQLError> {
                Ok(crate::CommandStoredDocument {
                    fields,
                    metadata: DocumentMetadata::with_tuple_xmin(self.tuple_version_xid()?),
                })
            })
            .transpose()?;
        let mut overlays = self.session.command_mutation_overlays.lock();
        let overlay = overlays.last_mut().ok_or_else(|| {
            SQLError::Internal("stage document without an active command overlay".into())
        })?;
        let previous = overlay
            .documents
            .get(&table)
            .and_then(|documents| documents.get(&doc_id))
            .cloned();
        let index_updates = overlay
            .exact_indexes
            .get(&table)
            .map(|indexes| {
                indexes
                    .keys()
                    .map(|fields| {
                        Ok((
                            fields.clone(),
                            previous
                                .as_ref()
                                .and_then(Option::as_ref)
                                .map(|document| {
                                    command_exact_document_key(document.fields.as_ref(), fields)
                                })
                                .transpose()?,
                            document
                                .as_ref()
                                .map(|document| {
                                    command_exact_document_key(document.fields.as_ref(), fields)
                                })
                                .transpose()?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()
            })
            .transpose()?
            .unwrap_or_default();
        for (fields, previous_key, new_key) in index_updates {
            let index = overlay
                .exact_indexes
                .get_mut(&table)
                .and_then(|indexes| indexes.get_mut(&fields))
                .ok_or_else(|| {
                    SQLError::Internal("command-overlay exact index disappeared".into())
                })?;
            if let Some(previous_key) = previous_key {
                let empty = index
                    .doc_ids_by_key
                    .get_mut(&previous_key)
                    .is_some_and(|doc_ids| {
                        doc_ids.remove(&doc_id);
                        doc_ids.is_empty()
                    });
                if empty {
                    index.doc_ids_by_key.remove(&previous_key);
                }
            }
            if let Some(new_key) = new_key {
                index
                    .doc_ids_by_key
                    .entry(new_key)
                    .or_default()
                    .insert(doc_id);
            }
        }
        overlay
            .documents
            .entry(table)
            .or_default()
            .insert(doc_id, document);
        Ok(())
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
                    .documents
                    .get(&table)
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
    ) -> Result<Option<DocId>, SQLError> {
        let table = self.command_overlay_table_name(table)?;
        let (fields, key) = command_exact_lookup_parts(fields, values)?;
        let mut overlays = self.session.command_mutation_overlays.lock();
        for overlay in overlays.iter_mut() {
            let indexes = overlay.exact_indexes.entry(table.clone()).or_default();
            if !indexes.contains_key(&fields) {
                let mut index = super::super::CommandExactIndex::default();
                if let Some(documents) = overlay.documents.get(&table) {
                    for (doc_id, document) in documents {
                        let Some(document) = document else {
                            continue;
                        };
                        index
                            .doc_ids_by_key
                            .entry(command_exact_document_key(
                                document.fields.as_ref(),
                                &fields,
                            )?)
                            .or_default()
                            .insert(*doc_id);
                    }
                }
                indexes.insert(fields.clone(), index);
            }
        }
        let candidates = overlays
            .iter()
            .filter_map(|overlay| overlay.exact_indexes.get(&table))
            .filter_map(|indexes| indexes.get(&fields))
            .filter_map(|index| index.doc_ids_by_key.get(&key))
            .flatten()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for doc_id in candidates {
            let visible = overlays.iter().rev().find_map(|overlay| {
                overlay
                    .documents
                    .get(&table)
                    .and_then(|documents| documents.get(&doc_id))
            });
            if let Some(Some(document)) = visible {
                if command_exact_document_key(document.fields.as_ref(), &fields)? == key {
                    return Ok(Some(doc_id));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<BTreeMap<DocId, Option<StoredDocument>>>, SQLError> {
        let canonical = self.command_overlay_table_name(table)?;
        let mut changes = self
            .fixed_transaction_row_changes(&canonical)?
            .unwrap_or_default();
        let overlays = self.session.command_mutation_overlays.lock();
        if overlays.is_empty() && changes.is_empty() {
            return Ok(None);
        }
        for overlay in overlays.iter() {
            if let Some(documents) = overlay.documents.get(&canonical) {
                changes.extend(documents.iter().map(|(doc_id, document)| {
                    (
                        *doc_id,
                        document.as_ref().map(|document| {
                            StoredDocument::with_metadata(
                                document.fields.as_ref().clone(),
                                document.metadata,
                            )
                        }),
                    )
                }));
            }
        }
        Ok(Some(changes))
    }

    pub(crate) fn fixed_transaction_row_changes(
        &self,
        canonical_table: &str,
    ) -> Result<Option<BTreeMap<DocId, Option<StoredDocument>>>, SQLError> {
        let mut changes = self
            .query_transaction_overlay
            .as_ref()
            .and_then(|overlay| overlay.get(canonical_table).cloned())
            .unwrap_or_default();
        if self.query_transaction_overlay.is_some() && self.query_transaction_origin.is_none() {
            return Ok((!changes.is_empty()).then_some(changes));
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
            return Ok((!changes.is_empty()).then_some(changes));
        };
        let desired = {
            let stack = self.session.transactions.lock();
            if self.query_transaction_overlay.is_none()
                && stack
                    .first()
                    .is_none_or(|frame| frame.fixed_snapshot.is_none())
            {
                return Ok(None);
            }
            let mut desired = BTreeMap::new();
            for change in stack.iter().flat_map(|frame| frame.row_changes.iter()) {
                if self
                    .query_transaction_origin
                    .is_some_and(|origin| change.query_origin != Some(origin))
                {
                    continue;
                }
                if change.source_generation == generation {
                    desired.insert(
                        change.pending.key.doc_id,
                        !matches!(
                            change.pending.kind,
                            crate::row_locks::PendingRowChangeKind::Delete
                                | crate::row_locks::PendingRowChangeKind::Rewrite(_)
                        ),
                    );
                }
                if let crate::row_locks::PendingRowChangeKind::Rewrite(successor) =
                    change.pending.kind
                {
                    if change.successor_generation == Some(generation) {
                        desired.insert(successor.doc_id, true);
                    }
                }
            }
            desired
        };
        if desired.is_empty() {
            return Ok((!changes.is_empty()).then_some(changes));
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
        let present = desired
            .iter()
            .filter_map(|(doc_id, present)| present.then_some(*doc_id))
            .collect::<Vec<_>>();
        let documents = live
            .document_store
            .read()
            .get_stored_many(&present)
            .map_err(|error| {
                document_store_read_error("read fixed-snapshot transaction changes", &error)
            })?;
        changes.extend(desired.into_iter().map(|(doc_id, present)| {
            let document = present.then(|| documents.get(&doc_id).cloned()).flatten();
            (doc_id, document)
        }));
        Ok(Some(changes))
    }
}
