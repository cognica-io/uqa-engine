//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document lookup, conflict probing, mutation, rewrite, and deletion.

use super::{
    document_store_read_error, document_store_write_error, BTreeMap, DocId, Document, Engine,
    FieldName, IndexConflictProbe, SQLError, TableState, Value,
};

impl Engine {
    pub fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        let t = self.require_table(table)?;
        let result = t.document_store.read().get(doc_id);
        result.map_err(|error| document_store_read_error("read document", &error))
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
        let result = t.document_store.read().get_fields_multi(doc_ids, fields);
        result.map_err(|error| document_store_read_error("read document fields", &error))
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        let t = self.require_table(table)?;
        let rows = t
            .document_store
            .read()
            .get_fields_multi(doc_ids, &[field])
            .map_err(|error| document_store_read_error("read document field", &error))?;
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
        let t = self.require_table(table)?;
        let result = t.document_store.read().find_doc_id_by_field(field, value);
        result.map_err(|error| document_store_read_error("find document by field", &error))
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
        let maps_to_doc_id = columns.iter().any(|col| {
            col.name == column
                && col.primary_key
                && matches!(col.ty, uqa_sql::ast::ColumnType::Integer)
        });
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
        self.with_implicit_transaction(|engine| {
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
        self.with_implicit_transaction(|engine| {
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

    pub(crate) fn rewrite_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.rewrite_document_inner(table, doc_id, document)
        })
    }

    pub(super) fn rewrite_document_inner(
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
        let vectors = Self::document_vector_values(&t, &document)?;
        self.validate_vector_values(&table_name, &vectors)?;
        self.add_document_with_vector_values_inner(&table_name, doc_id, document, vectors, false)
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
        Ok(())
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.delete_document_inner(table, doc_id))
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
        Ok(())
    }

    pub fn document_count(&self, table: &str) -> Result<u64, SQLError> {
        let t = self.require_table(table)?;
        let result = t.inverted_index.read().doc_count();
        result.map_err(|error| SQLError::Internal(format!("read indexed document count: {error}")))
    }
}
