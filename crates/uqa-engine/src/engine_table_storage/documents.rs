//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document lookup, conflict probing, mutation, rewrite, and deletion.

use super::{
    document_store_read_error, document_store_write_error, Arc, BTreeMap, DocId, Document, Engine,
    FieldName, IndexConflictProbe, SQLError, TableState, Value,
};

enum CommandOverlayDocument {
    Present(Document),
    Deleted,
}

fn command_exact_lookup_parts(
    fields: &[String],
    values: &[Value],
) -> Result<(Vec<String>, Vec<u8>), SQLError> {
    if fields.len() != values.len() {
        return Err(SQLError::Internal(
            "command-overlay exact lookup has mismatched fields and values".into(),
        ));
    }
    let mut pairs = fields
        .iter()
        .cloned()
        .zip(values.iter().cloned())
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    let (fields, values): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    let key = uqa_execution::canonical_row_key(&values).map_err(|error| {
        SQLError::Internal(format!("encode command-overlay exact lookup key: {error}"))
    })?;
    Ok((fields, key))
}

fn command_exact_document_key(document: &Document, fields: &[String]) -> Result<Vec<u8>, SQLError> {
    let values = fields
        .iter()
        .map(|field| document.get(field).cloned().unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    uqa_execution::canonical_row_key(&values).map_err(|error| {
        SQLError::Internal(format!("encode command-overlay document key: {error}"))
    })
}

impl Engine {
    fn command_overlay_table_name(&self, table: &str) -> Result<String, SQLError> {
        self.try_resolve_table_name(table)
            .map_err(|error| {
                SQLError::Internal(format!("resolve command-overlay table `{table}`: {error}"))
            })
            .map(|resolved| resolved.unwrap_or_else(|| table.to_string()))
    }

    pub(crate) fn begin_command_mutation_overlay(&self) {
        self.session
            .command_mutation_overlays
            .lock()
            .push(super::CommandMutationOverlay::default());
    }

    pub(crate) fn end_command_mutation_overlay(&self) {
        let removed = self.session.command_mutation_overlays.lock().pop();
        debug_assert!(
            removed.is_some(),
            "command mutation overlay stack underflow"
        );
    }

    pub(crate) fn command_mutation_overlay_active(&self) -> bool {
        !self.session.command_mutation_overlays.lock().is_empty()
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
                                .map(|document| command_exact_document_key(document, fields))
                                .transpose()?,
                            document
                                .as_ref()
                                .map(|document| command_exact_document_key(document, fields))
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

    fn command_overlay_document(
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
                            CommandOverlayDocument::Present(document.as_ref().clone())
                        }
                        None => CommandOverlayDocument::Deleted,
                    })
            }))
    }

    fn command_overlay_exact_match(
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
                let mut index = super::CommandExactIndex::default();
                if let Some(documents) = overlay.documents.get(&table) {
                    for (doc_id, document) in documents {
                        let Some(document) = document else {
                            continue;
                        };
                        index
                            .doc_ids_by_key
                            .entry(command_exact_document_key(document, &fields)?)
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
                if command_exact_document_key(document, &fields)? == key {
                    return Ok(Some(doc_id));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<BTreeMap<DocId, Option<Document>>>, SQLError> {
        let canonical = self.command_overlay_table_name(table)?;
        let overlays = self.session.command_mutation_overlays.lock();
        if overlays.is_empty() {
            return Ok(None);
        }
        let mut changes = BTreeMap::new();
        for overlay in overlays.iter() {
            if let Some(documents) = overlay.documents.get(&canonical) {
                changes.extend(documents.iter().map(|(doc_id, document)| {
                    (
                        *doc_id,
                        document.as_ref().map(|document| document.as_ref().clone()),
                    )
                }));
            }
        }
        Ok(Some(changes))
    }

    fn raw_command_visible_document(
        &self,
        table: &str,
        state: &TableState,
        doc_id: DocId,
    ) -> Result<Option<Document>, SQLError> {
        match self.command_overlay_document(table, doc_id)? {
            Some(CommandOverlayDocument::Present(document)) => Ok(Some(document)),
            Some(CommandOverlayDocument::Deleted) => Ok(None),
            None => state
                .document_store
                .read()
                .get(doc_id)
                .map_err(|error| document_store_read_error("read document", &error)),
        }
    }

    pub fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        let t = self.require_table(table)?;
        let mut document = self.raw_command_visible_document(table, &t, doc_id)?;
        if let Some(document) = document.as_mut() {
            crate::engine_generated::materialize_virtual_generated_columns(
                &t.columns.read(),
                document,
            )?;
        }
        Ok(document)
    }

    /// Read the latest committed tuple through an independent persistent session while this session keeps its statement snapshot pinned.
    pub(crate) fn get_committed_document(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<Document>, SQLError> {
        let Some(provider) = self.storage.provider.as_ref() else {
            return self.get_document(table, doc_id);
        };
        let canonical = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .unwrap_or_else(|| table.to_string());
        let session = provider.open_session().map_err(|error| {
            SQLError::Internal(format!(
                "open independent session to recheck `{canonical}` row {doc_id}: {error}"
            ))
        })?;
        let mut document = session
            .backend
            .document_store(&canonical)
            .get(doc_id)
            .map_err(|error| document_store_read_error("read latest committed document", &error))?;
        if let Some(document) = document.as_mut() {
            let table = self.require_table(&canonical)?;
            crate::engine_generated::materialize_virtual_generated_columns(
                &table.columns.read(),
                document,
            )?;
        }
        Ok(document)
    }

    /// Execute a retrieval predicate against the latest committed index state through an independent session while this session keeps its statement snapshot pinned. A tuple-local recheck uses it so a substituted committed image is judged by the retrieval predicate the way `PostgreSQL` re-evaluates the WHERE clause on the new tuple. Without a provider the engine has a single shared state already.
    pub(crate) fn committed_retrieval_entries(
        &self,
        table: &str,
        predicate: &uqa_execution::ScalarExpr,
        params: &[uqa_sql::SQLParam],
    ) -> Result<Option<Vec<crate::ScoredEntry>>, SQLError> {
        if self.storage.provider.is_none() {
            return crate::operator_tree_bridge::run_optimised(
                self,
                table,
                Some(predicate),
                params,
            );
        }
        let session = self.new_session().map_err(|error| {
            SQLError::Internal(format!(
                "open independent session to recheck retrieval on `{table}`: {error}"
            ))
        })?;
        crate::operator_tree_bridge::run_optimised(&session, table, Some(predicate), params)
    }

    /// Fetch complete physical documents while materialising only the virtual
    /// generated columns named by `projection`. Callers that need full rows
    /// should use [`Engine::get_document`]; projected execution paths use this
    /// boundary so unrelated virtual expressions remain deferred.
    pub(crate) fn get_documents_with_virtual_projection(
        &self,
        table: &str,
        doc_ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError> {
        let t = self.require_table(table)?;
        let columns = t.columns.read().clone();
        let mut documents = t.document_store.read().get_many(doc_ids).map_err(|error| {
            document_store_read_error("read generated document projection", &error)
        })?;
        if let Some(changes) = self.command_overlay_changes(table)? {
            for doc_id in doc_ids {
                let Some(document) = changes.get(doc_id) else {
                    continue;
                };
                if let Some(document) = document {
                    documents.insert(*doc_id, document.clone());
                } else {
                    documents.remove(doc_id);
                }
            }
        }
        for document in documents.values_mut() {
            crate::engine_generated::materialize_projected_virtual_generated_columns(
                &columns, document, projection,
            )?;
        }
        Ok(documents)
    }

    /// Fetch a column projection for many documents in one round trip.
    /// The value vector aligns with `fields`; missing fields are Null.
    /// Persistent backends extract the fields inside the storage scan
    /// so whole documents never materialise.
    pub(crate) fn get_document_fields_multi(
        &self,
        table: &str,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> Result<BTreeMap<DocId, Vec<Value>>, SQLError> {
        let t = self.require_table(table)?;
        let columns = t.columns.read().clone();
        let requested = fields
            .iter()
            .map(|field| (*field).to_string())
            .collect::<Vec<_>>();
        if crate::engine_generated::projection_contains_virtual_generated_column(
            &columns, &requested,
        ) {
            let documents =
                self.get_documents_with_virtual_projection(table, doc_ids, &requested)?;
            let mut projected = BTreeMap::new();
            for (doc_id, document) in documents {
                projected.insert(
                    doc_id,
                    fields
                        .iter()
                        .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                        .collect(),
                );
            }
            return Ok(projected);
        }
        if let Some(changes) = self.command_overlay_changes(table)? {
            let persisted_ids = doc_ids
                .iter()
                .filter(|doc_id| !changes.contains_key(doc_id))
                .copied()
                .collect::<Vec<_>>();
            let mut projected = t
                .document_store
                .read()
                .get_fields_multi(&persisted_ids, fields)
                .map_err(|error| document_store_read_error("read document fields", &error))?;
            for doc_id in doc_ids {
                if let Some(Some(document)) = changes.get(doc_id) {
                    projected.insert(
                        *doc_id,
                        fields
                            .iter()
                            .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                            .collect(),
                    );
                }
            }
            return Ok(projected);
        }
        let result = t.document_store.read().get_fields_multi(doc_ids, fields);
        result.map_err(|error| document_store_read_error("read document fields", &error))
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        let rows = self.get_document_fields_multi(table, doc_ids, &[field])?;
        let mut out = BTreeMap::new();
        for (doc_id, mut values) in rows {
            if values.len() != 1 {
                return Err(SQLError::Internal(format!(
                    "read document field returned {} projected values for document {doc_id}; expected 1",
                    values.len()
                )));
            }
            out.insert(doc_id, values.remove(0));
        }
        Ok(out)
    }

    pub fn find_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError> {
        if let Some(doc_id) = self.command_overlay_exact_match(
            table,
            &[field.to_string()],
            std::slice::from_ref(value),
        )? {
            return Ok(Some(doc_id));
        }
        let t = self.require_table(table)?;
        let Some(changes) = self.command_overlay_changes(table)? else {
            return t
                .document_store
                .read()
                .find_doc_id_by_field(field, value)
                .map_err(|error| document_store_read_error("find document by field", &error));
        };
        if changes.is_empty() {
            return t
                .document_store
                .read()
                .find_doc_id_by_field(field, value)
                .map_err(|error| document_store_read_error("find document by field", &error));
        }
        let store = t.document_store.read();
        let mut after = None;
        loop {
            let doc_ids = store
                .next_doc_ids(after, uqa_execution::DEFAULT_BATCH_SIZE)
                .map_err(|error| {
                    document_store_read_error("scan command-visible document fields", &error)
                })?;
            let Some(last) = doc_ids.last().copied() else {
                return Ok(None);
            };
            after = Some(last);
            let projected = store
                .get_fields_multi(&doc_ids, &[field])
                .map_err(|error| {
                    document_store_read_error("read command-visible document field", &error)
                })?;
            for doc_id in doc_ids {
                if changes.contains_key(&doc_id) {
                    continue;
                }
                if projected
                    .get(&doc_id)
                    .and_then(|values| values.first())
                    .unwrap_or(&Value::Null)
                    == value
                {
                    return Ok(Some(doc_id));
                }
            }
        }
    }

    /// Find the first document whose conflict columns all match the
    /// given values. Returns the existing doc id when a conflict
    /// exists, `None` when the row would be a fresh insert. Mirrors
    /// `PostgreSQL`'s `ON CONFLICT (col, ...)` lookup; the conflict
    /// columns map to the unique-constraint target.
    ///
    /// Lookup order: the integer-primary-key slot mapping, then a
    /// value-index equality probe on the first index-answerable
    /// conflict column (conflict targets are PRIMARY KEY / UNIQUE
    /// columns admitted by `value_indexable_fields`), and only then the
    /// evaluated document scan. The index
    /// probe is what keeps per-row UNIQUE and FOREIGN KEY validation
    /// `O(log n)` during bulk inserts -- previously every insert into a
    /// table with a non-integer unique column re-scanned all documents,
    /// making an n-row load `O(n^2)`.
    pub fn find_conflict(
        &self,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        if conflict_columns.is_empty() || conflict_columns.len() != values.len() {
            return Ok(None);
        }
        if let Some(doc_id) = self.command_overlay_exact_match(table, conflict_columns, values)? {
            return Ok(Some(doc_id));
        }
        let persisted = self.find_persisted_conflict(table, conflict_columns, values)?;
        let Some(doc_id) = persisted else {
            return Ok(None);
        };
        Ok(self
            .command_overlay_document(table, doc_id)?
            .is_none()
            .then_some(doc_id))
    }

    fn find_persisted_conflict(
        &self,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        let t = self.require_table(table)?;
        if conflict_columns.len() == 1 {
            if let Some(doc_id) =
                Self::doc_id_for_primary_key_conflict(&t, &conflict_columns[0], &values[0])
            {
                if u128::from(doc_id) >= *t.next_id.lock() {
                    return Ok(None);
                }
                let exists = t
                    .document_store
                    .read()
                    .contains_doc_id(doc_id)
                    .map_err(|error| {
                        document_store_read_error("check conflicting document", &error)
                    })?;
                return Ok(exists.then_some(doc_id));
            }
        }
        match self.find_conflict_via_value_index(&t, table, conflict_columns, values)? {
            IndexConflictProbe::Conflict(doc_id) => return Ok(Some(doc_id)),
            IndexConflictProbe::NoConflict => return Ok(None),
            IndexConflictProbe::Unanswerable => {}
        }
        let result = t
            .document_store
            .read()
            .find_doc_id_by_fields(conflict_columns, values);
        result.map_err(|error| document_store_read_error("find conflicting document", &error))
    }

    /// Index-backed conflict lookup. `Unanswerable` means no conflict
    /// column could be answered by a value index (unindexed columns, or
    /// the temporal/NaN semantics guard refused) and the caller must
    /// fall back to the evaluated scan. Otherwise the answer is
    /// authoritative: candidates narrow through the pivot column's
    /// posting list in `O(log n + k)` and the remaining columns verify
    /// against stored fields on those candidates only, with the same
    /// `Value` equality the evaluated scan uses. An empty posting list
    /// is an authoritative `NoConflict`, which is the common case on
    /// insert and must not degrade into a scan.
    pub(super) fn find_conflict_via_value_index(
        &self,
        t: &TableState,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Result<IndexConflictProbe, SQLError> {
        for (pivot, (column, value)) in conflict_columns.iter().zip(values.iter()).enumerate() {
            let Some(candidates) =
                self.value_index_scan(table, column, &uqa_core::Predicate::Equals(value.clone()))?
            else {
                continue;
            };
            let store = t.document_store.read();
            for entry in candidates.entries() {
                let mut matches = true;
                for (index, (column, expected)) in
                    conflict_columns.iter().zip(values.iter()).enumerate()
                {
                    if index == pivot {
                        continue;
                    }
                    let actual = store.get_field(entry.doc_id, column).map_err(|error| {
                        document_store_read_error("verify conflicting document", &error)
                    })?;
                    if actual.unwrap_or(Value::Null) != *expected {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return Ok(IndexConflictProbe::Conflict(entry.doc_id));
                }
            }
            return Ok(IndexConflictProbe::NoConflict);
        }
        Ok(IndexConflictProbe::Unanswerable)
    }

    pub(super) fn doc_id_for_primary_key_conflict(
        table: &TableState,
        column: &str,
        value: &Value,
    ) -> Option<DocId> {
        let Value::Int(id) = value else {
            return None;
        };
        if *id < 0 {
            return None;
        }
        let columns = table.columns.read();
        let maps_to_doc_id = columns
            .iter()
            .any(|col| col.name == column && col.primary_key && col.ty.is_integer());
        if !maps_to_doc_id {
            return None;
        }
        Some(*id as DocId)
    }

    /// Apply per-column updates to an existing document. Mirrors the
    /// `DO UPDATE SET col = expr` branch of an ON CONFLICT clause.
    /// Returns whether the row was updated; `Ok(false)` when the
    /// document no longer exists. Storage write failures surface as
    /// `Err` so the enclosing transaction rolls back instead of
    /// committing a delete whose re-insert never happened.
    pub fn update_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.update_document_fields_with_vector_values(table, doc_id, updates, vector_values)
    }

    pub fn update_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let columns = updates.keys().cloned().collect::<Vec<_>>();
        let strength = crate::sql::dml::update_lock_strength(self, table, &columns);
        self.with_implicit_row_write_transaction(table, doc_id, strength, |engine| {
            engine.update_document_fields_with_vector_values_inner(table, doc_id, updates, vectors)
        })
    }

    pub(super) fn update_document_fields_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(mut doc) = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|error| document_store_read_error("read document for update", &error))?
        else {
            return Ok(false);
        };
        self.validate_vector_values(table, &vectors)?;
        for (k, v) in updates {
            doc.insert(k, v);
        }
        let mut replacement_vectors = Self::document_vector_values(&t, &doc)?;
        for (field, values) in vectors {
            replacement_vectors.insert(field, values);
        }
        // Each index's replacement path validates/stages before publishing.
        // Never delete the old row/index state first: an analyzer or backend
        // failure must leave the prior version queryable.
        self.add_document_with_vector_values_inner(table, doc_id, doc, replacement_vectors, false)?;
        Ok(true)
    }

    /// Apply field-level updates without materialising the whole
    /// document. Callers must only use this path when constraints and
    /// referential actions do not need the old or complete new row.
    pub fn patch_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values: BTreeMap<String, Vec<Vec<f32>>> = vectors
            .iter()
            .map(|(field, vector)| (field.clone(), vec![vector.clone()]))
            .collect();
        self.patch_document_fields_with_vector_values(table, doc_id, updates, &vector_values)
    }

    pub fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let columns = updates.keys().cloned().collect::<Vec<_>>();
        let strength = crate::sql::dml::update_lock_strength(self, table, &columns);
        self.with_implicit_row_write_transaction(table, doc_id, strength, |engine| {
            engine.patch_document_fields_with_vector_values_inner(table, doc_id, updates, vectors)
        })
    }

    pub(super) fn patch_document_fields_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(mut document) = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|error| document_store_read_error("read document for update", &error))?
        else {
            return Ok(false);
        };
        self.validate_vector_values(table, vectors)?;
        for (field, value) in updates {
            if matches!(value, Value::Null) {
                document.remove(field);
            } else {
                document.insert(field.clone(), value.clone());
            }
        }

        let vector_fields = t.vector_indexes.read().keys().cloned().collect::<Vec<_>>();
        let mut replacement_vectors = vectors.clone();
        for field in vector_fields {
            if !updates.contains_key(&field) || replacement_vectors.contains_key(&field) {
                continue;
            }
            let values = match document.get(&field) {
                Some(value) => Self::field_index_vectors(&t, &field, value)?.unwrap_or_default(),
                None => Vec::new(),
            };
            replacement_vectors.insert(field, values);
        }

        // The common replacement path stages text analysis and vector input
        // before publishing and updates the document/value indexes as one
        // logical row version. This avoids the old patch -> remove -> add
        // sequence where an analyzer failure left the stored row changed and
        // its postings deleted in a memory engine.
        self.add_document_with_vector_values_inner(
            table,
            doc_id,
            document,
            replacement_vectors,
            false,
        )?;
        Ok(true)
    }

    /// Rewrite one row for SQL `UPDATE`. The caller has already acquired the tuple lock at the strength `update_lock_strength` derived from the changed columns and holds the relation lock, so this path must not re-lock: taking `FOR UPDATE` here would make a non-key update conflict with concurrent `FOR KEY SHARE` holders and publish an inflated mutation strength, both contrary to `PostgreSQL` 18.
    pub(crate) fn rewrite_prepared_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let table_state = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let vectors = Self::document_vector_values(&table_state, &document)?;
        self.with_implicit_transaction(|engine| {
            engine.add_prepared_document_with_vector_values_inner(
                &table_name,
                doc_id,
                document,
                vectors,
                false,
            )
        })
    }

    /// Rewrite a row while a column is being dropped or renamed. The
    /// operation changes field names, not the indexed values: catalog
    /// lifecycle code drops or renames the durable postings afterward.
    /// Maintaining them against the half-updated schema here would replace a
    /// renamed field with NULL before its metadata has moved.
    pub(super) fn rewrite_document_for_schema_change(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let vector_fields: Vec<FieldName> = t.vector_indexes.read().keys().cloned().collect();
        let mut vectors: BTreeMap<FieldName, Vec<Vec<f32>>> = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(&t, &field, value)? {
                vectors.insert(field, values);
            }
        }
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for field in t.fts_fields() {
            if let Some(Value::Str(value)) = document.get(&field) {
                text_fields.insert(field, value.clone());
            }
        }
        t.document_store
            .write()
            .put(doc_id, document)
            .map_err(|err| document_store_write_error(&err))?;
        {
            let mut index = t.inverted_index.write();
            index
                .add_document(doc_id, text_fields)
                .map_err(|error| SQLError::Internal(format!("index document: {error}")))?;
        }
        for (field, index) in t.vector_indexes.write().iter_mut() {
            index
                .add_many(doc_id, vectors.remove(field).unwrap_or_default())
                .map_err(|error| SQLError::Internal(format!("index document vector: {error}")))?;
        }
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        self.note_row_changed(&table_name, doc_id)?;
        Ok(())
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| engine.delete_document_inner(table, doc_id),
        )
    }

    pub(super) fn delete_document_inner(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let existed = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|err| document_store_write_error(&err))?
            .is_some();
        let old_indexed = Self::value_indexes_old_values(&t, doc_id)?;
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(&table_name, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        t.inverted_index
            .write()
            .remove_document(doc_id)
            .map_err(|error| SQLError::Internal(format!("remove indexed document: {error}")))?;
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut()
                .delete(doc_id)
                .map_err(|error| SQLError::Internal(format!("delete indexed vector: {error}")))?;
        }
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        if existed {
            self.note_row_deleted(&table_name, doc_id)?;
        }
        Ok(())
    }

    pub fn document_count(&self, table: &str) -> Result<u64, SQLError> {
        let t = self.require_table(table)?;
        let result = t.inverted_index.read().doc_count();
        result.map_err(|error| SQLError::Internal(format!("read indexed document count: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(id: i64, value: i64) -> Document {
        BTreeMap::from([
            ("id".into(), Value::Int(id)),
            ("value".into(), Value::Int(value)),
        ])
    }

    #[test]
    fn command_overlay_scan_merges_persisted_and_staged_rows_in_document_order() {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE TABLE command_scan (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO command_scan VALUES (1, 10), (2, 20), (4, 40)",
                &[],
            )
            .unwrap();
        engine.begin_command_mutation_overlay();
        engine
            .stage_command_document("command_scan", 2, Some(document(2, 200)))
            .unwrap();
        engine
            .stage_command_document("command_scan", 3, Some(document(3, 300)))
            .unwrap();
        engine
            .stage_command_document("command_scan", 4, None)
            .unwrap();
        engine
            .stage_command_document("command_scan", 5, Some(document(5, 500)))
            .unwrap();

        let result = engine
            .sql("SELECT id, value FROM command_scan ORDER BY id", &[])
            .unwrap();

        assert_eq!(engine.table_doc_count("command_scan").unwrap(), 4);
        assert_eq!(engine.table_doc_ids("command_scan").unwrap(), [1, 2, 3, 5]);
        assert_eq!(result.rows.len(), 4);
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(10)));
        assert_eq!(result.value_at(1, 1), Some(&Value::Int(200)));
        assert_eq!(result.value_at(2, 1), Some(&Value::Int(300)));
        assert_eq!(result.value_at(3, 1), Some(&Value::Int(500)));
        engine.end_command_mutation_overlay();
    }

    #[test]
    fn command_overlay_scan_pages_without_losing_filtered_or_changed_rows() {
        use std::sync::atomic::Ordering;

        let engine = Engine::new();
        engine
            .sql(
                "CREATE TABLE paged_command_scan (id INTEGER PRIMARY KEY, value INTEGER)",
                &[],
            )
            .unwrap();
        let table = engine.require_table("paged_command_scan").unwrap();
        {
            let mut store = table.document_store.write();
            for doc_id in 1..=2050 {
                let value = i64::try_from(doc_id).unwrap();
                store.put(doc_id, document(value, value)).unwrap();
            }
        }
        table.doc_count_dirty.store(true, Ordering::Release);
        engine.begin_command_mutation_overlay();
        engine
            .stage_command_document("paged_command_scan", 2, Some(document(2, 9002)))
            .unwrap();
        engine
            .stage_command_document("paged_command_scan", 1025, Some(document(1025, 9025)))
            .unwrap();
        engine
            .stage_command_document("paged_command_scan", 2048, None)
            .unwrap();
        engine
            .stage_command_document("paged_command_scan", 4096, Some(document(4096, 9999)))
            .unwrap();

        let ids = engine
            .sql(
                "SELECT id FROM paged_command_scan WHERE value >= 2047 ORDER BY id",
                &[],
            )
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            [2, 1025, 2047, 2049, 2050, 4096].map(Value::Int).to_vec()
        );
        engine.end_command_mutation_overlay();
    }

    #[test]
    fn overlay_projection_materializes_only_requested_virtual_columns() {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE TABLE overlay_virtual (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (1 / source) VIRTUAL); INSERT INTO overlay_virtual (id, source) VALUES (1, 1), (2, 2)",
                &[],
            )
            .unwrap();
        engine.begin_command_mutation_overlay();
        engine
            .stage_command_document(
                "overlay_virtual",
                1,
                Some(BTreeMap::from([
                    ("id".into(), Value::Int(1)),
                    ("source".into(), Value::Int(0)),
                ])),
            )
            .unwrap();
        engine
            .stage_command_document("overlay_virtual", 2, None)
            .unwrap();
        engine
            .stage_command_document(
                "overlay_virtual",
                3,
                Some(BTreeMap::from([
                    ("id".into(), Value::Int(3)),
                    ("source".into(), Value::Int(3)),
                ])),
            )
            .unwrap();

        let documents = engine
            .get_documents_with_virtual_projection(
                "overlay_virtual",
                &[1, 2, 3],
                &["source".into()],
            )
            .unwrap();
        assert_eq!(documents[&1]["source"], Value::Int(0));
        assert!(!documents.contains_key(&2));
        assert_eq!(documents[&3]["source"], Value::Int(3));
        let fields = engine
            .get_document_fields_multi("overlay_virtual", &[1, 2, 3], &["source"])
            .unwrap();
        assert_eq!(fields[&1], [Value::Int(0)]);
        assert!(!fields.contains_key(&2));
        assert_eq!(fields[&3], [Value::Int(3)]);
        assert!(engine
            .get_documents_with_virtual_projection("overlay_virtual", &[1], &["derived".into()],)
            .is_err());
        engine.end_command_mutation_overlay();
    }
}
