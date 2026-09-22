//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document lookup, conflict probing, mutation, rewrite, and deletion.

use super::{
    document_store_read_error, document_store_write_error, Arc, BTreeMap, DocId, Document, Engine,
    FieldName, SQLError, TableState, Value,
};
use uqa_storage::{DocumentMetadata, DocumentStore, StoredDocument};

enum CommandOverlayDocument {
    Present(uqa_storage::StoredDocument),
    Deleted,
}

mod exact_lookup;
mod overlay;

impl Engine {
    pub(super) fn raw_command_visible_document(
        &self,
        table: &str,
        state: &TableState,
        doc_id: DocId,
    ) -> Result<Option<StoredDocument>, SQLError> {
        match self.command_overlay_document(table, doc_id)? {
            Some(CommandOverlayDocument::Present(document)) => Ok(Some(document)),
            Some(CommandOverlayDocument::Deleted) => Ok(None),
            None => state
                .document_store
                .read()
                .get_stored(doc_id)
                .map_err(|error| document_store_read_error("read document", &error)),
        }
    }

    pub(super) fn materialize_query_document(
        columns: &[uqa_sql::ast::ColumnDef],
        document: &mut StoredDocument,
    ) -> Result<(), SQLError> {
        crate::generated::materialize_virtual_generated_columns(columns, document.fields_mut())?;
        if uqa_execution::query::document_projection::projection_uses_tuple_xmin(
            uqa_sql::semantics::XMIN_COLUMN,
            columns,
        ) && !document
            .fields()
            .contains_key(uqa_sql::semantics::XMIN_COLUMN)
        {
            let xmin = document
                .metadata()
                .tuple_xmin()
                .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)));
            document
                .fields_mut()
                .insert(uqa_sql::semantics::XMIN_COLUMN.into(), xmin);
        }
        Ok(())
    }

    /// Read only user fields for a tuple rewrite. A successful rewrite receives fresh tuple metadata at the storage publication boundary.
    pub(crate) fn get_document_for_mutation(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<Document>, SQLError> {
        let state = self.require_table(table)?;
        let mut document = self.raw_command_visible_document(table, &state, doc_id)?;
        if let Some(document) = document.as_mut() {
            crate::generated::materialize_virtual_generated_columns(
                &state.columns.read(),
                document.fields_mut(),
            )?;
        }
        Ok(document.map(StoredDocument::into_fields))
    }

    pub(crate) fn get_query_document_metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<DocumentMetadata>, SQLError> {
        let table_state = self.require_query_table(table)?;
        self.raw_command_visible_document(table, &table_state, doc_id)
            .map(|document| document.map(|document| document.metadata()))
    }

    /// Read the latest committed tuple through an independent persistent session while this session keeps its statement snapshot pinned.
    pub(crate) fn get_committed_document(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<StoredDocument>, SQLError> {
        let Some(provider) = self.storage.provider.as_ref() else {
            let state = self.require_table(table)?;
            let mut document = self.raw_command_visible_document(table, &state, doc_id)?;
            if let Some(document) = document.as_mut() {
                crate::generated::materialize_virtual_generated_columns(
                    &state.columns.read(),
                    document.fields_mut(),
                )?;
            }
            return Ok(document);
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
            .get_stored(doc_id)
            .map_err(|error| document_store_read_error("read latest committed document", &error))?;
        if let Some(document) = document.as_mut() {
            let table = self.require_table(&canonical)?;
            crate::generated::materialize_virtual_generated_columns(
                &table.columns.read(),
                document.fields_mut(),
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
        let session = self.new_internal_read_session().map_err(|error| {
            SQLError::Internal(format!(
                "open independent session to recheck retrieval on `{table}`: {error}"
            ))
        })?;
        crate::operator_tree_bridge::run_optimised(&session, table, Some(predicate), params)
    }

    /// Execute one raw KNN leaf against the latest committed vector-index state for a tuple-local hierarchy recheck.
    pub(crate) fn committed_knn_entries(
        &self,
        table: &str,
        field: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<crate::ScoredEntry>, SQLError> {
        if self.storage.provider.is_none() {
            return self.knn_search_leaf(table, field, query_vector, top_k);
        }
        let session = self.new_internal_read_session().map_err(|error| {
            SQLError::Internal(format!(
                "open independent session to recheck vector retrieval on `{table}`: {error}"
            ))
        })?;
        session.knn_search_leaf(table, field, query_vector, top_k)
    }

    /// Fetch complete physical documents while materializing only the virtual generated or storage-owned tuple columns named by `projection`; projected execution paths use this boundary so unrelated virtual expressions remain deferred.
    pub(crate) fn get_documents_with_materialized_projection(
        &self,
        table: &str,
        doc_ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError> {
        let t = self.require_table(table)?;
        let columns = t.columns.read().clone();
        let mut documents = t
            .document_store
            .read()
            .get_stored_many(doc_ids)
            .map_err(|error| {
                document_store_read_error("read generated document projection", &error)
            })?;
        if let Some(changes) = self.command_overlay_changes(table)? {
            for doc_id in doc_ids {
                if !changes.contains_change(*doc_id) {
                    continue;
                }
                if let Some(document) = changes.get_stored(*doc_id).map_err(|error| {
                    document_store_read_error("read private generated document projection", &error)
                })? {
                    documents.insert(*doc_id, document);
                } else {
                    documents.remove(doc_id);
                }
            }
        }
        for document in documents.values_mut() {
            crate::generated::materialize_projected_virtual_generated_columns(
                &columns,
                document.fields_mut(),
                projection,
            )?;
            if uqa_execution::query::document_projection::projections_use_tuple_xmin(
                projection, &columns,
            ) && !document
                .fields()
                .contains_key(uqa_sql::semantics::XMIN_COLUMN)
            {
                let xmin = document
                    .metadata()
                    .tuple_xmin()
                    .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)));
                document
                    .fields_mut()
                    .insert(uqa_sql::semantics::XMIN_COLUMN.into(), xmin);
            }
        }
        Ok(documents
            .into_iter()
            .map(|(doc_id, document)| (doc_id, document.into_fields()))
            .collect())
    }

    pub(crate) fn get_query_document_fields_multi(
        &self,
        table: &str,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> Result<BTreeMap<DocId, Vec<Value>>, SQLError> {
        let table_state = self.require_query_table(table)?;
        let columns = table_state.columns.read().clone();
        let changes = self.command_overlay_changes(table)?;
        let persisted_ids = doc_ids
            .iter()
            .copied()
            .filter(|id| {
                changes
                    .as_ref()
                    .is_none_or(|changes| !changes.contains_change(*id))
            })
            .collect::<Vec<_>>();
        let mut projected = uqa_execution::query::document_projection::read_document_projection(
            &**table_state.document_store.read(),
            &persisted_ids,
            fields,
            &columns,
        )?;
        if let Some(changes) = changes {
            let private_ids = doc_ids
                .iter()
                .copied()
                .filter(|id| changes.change_presence(*id) == Some(true))
                .collect::<Vec<_>>();
            projected.extend(
                uqa_execution::query::document_projection::read_document_projection(
                    &changes,
                    &private_ids,
                    fields,
                    &columns,
                )?,
            );
        }
        Ok(projected)
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        let rows = self.get_query_document_fields_multi(table, doc_ids, &[field])?;
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
        let strength =
            uqa_execution::query::locking::context::update_lock_strength(self, table, &columns);
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
        if let Some(read) = self.serializable_table_state_read(&t)? {
            read.observe_row(doc_id)?;
        }
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
        let strength =
            uqa_execution::query::locking::context::update_lock_strength(self, table, &columns);
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
        if let Some(read) = self.serializable_table_state_read(&t)? {
            read.observe_row(doc_id)?;
        }
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
        self.with_prepared_row_write_transaction(&table_name, |engine| {
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
    pub(crate) fn rewrite_document_for_schema_change(
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
        {
            let mut store = t.document_store.write();
            let metadata = store
                .get_metadata(doc_id)
                .map_err(|err| document_store_read_error("read tuple metadata for schema rewrite", &err))?
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "table `{table_name}` listed document {doc_id} for a schema rewrite but omitted its tuple metadata"
                    ))
                })?;
            store
                .put_stored(doc_id, StoredDocument::with_metadata(document, metadata))
                .map_err(|err| document_store_write_error(&err))?;
        }
        uqa_execution::serializable::text::add_document(
            self,
            &table_name,
            t.columns.snapshot(),
            t.inverted_index.write().as_mut(),
            doc_id,
            text_fields,
        )?;
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
        if let Some(read) = self.serializable_table_state_read(&t)? {
            read.observe_row(doc_id)?;
        }
        let existed = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|err| document_store_write_error(&err))?
            .is_some();
        if existed {
            uqa_execution::serializable::observe_row_write(self, &table_name, doc_id)?;
        }
        let old_indexed = Self::value_indexes_old_values(&t, doc_id);
        self.observe_value_index_write(
            &table_name,
            &t,
            doc_id,
            existed,
            old_indexed.as_ref(),
            None,
        )?;
        for (field, index) in t.vector_indexes.read().iter() {
            uqa_execution::serializable::vector::observe_write(
                self,
                &table_name,
                &t.columns.snapshot(),
                field,
                index.as_ref(),
                doc_id,
                uqa_execution::serializable::vector::VectorChange::Delete,
            )?;
        }
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(&table_name, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        uqa_execution::serializable::text::remove_document(
            self,
            &table_name,
            t.columns.snapshot(),
            t.inverted_index.write().as_mut(),
            doc_id,
        )?;
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
}

#[cfg(test)]
mod tests;
