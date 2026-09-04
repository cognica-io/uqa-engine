//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, Arc, BTreeMap, DocId, Document, Engine, FieldName, FtsIndexStat, SQLError,
    TableState, Value,
};

impl Engine {
    pub(crate) fn fts_fields_for_table(&self, name: &str) -> Result<Vec<FieldName>, SQLError> {
        Ok(self
            .try_query_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?
            .map_or_else(Vec::new, |table| table.fts_fields()))
    }

    /// Validate the physical text-search contract for one concrete field.
    /// A declared TEXT column is not searchable until it has been registered
    /// in a GIN/FTS index; treating that state as an empty posting list hides a
    /// schema/configuration error from both the public search API and the
    /// operator-tree executor.
    pub(crate) fn validate_text_search_field(
        &self,
        table: &str,
        field: &str,
    ) -> Result<(), SQLError> {
        let Some(table_state) = self
            .try_query_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve text-search table: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        if table_state
            .fts_fields()
            .iter()
            .any(|indexed| indexed == field)
        {
            return Ok(());
        }

        let columns = table_state.columns.read();
        if !columns.is_empty() && !columns.iter().any(|column| column.name == field) {
            return Err(SQLError::UnknownColumn(field.to_string()));
        }
        Err(SQLError::TypeMismatch(format!(
            "text search: column `{table}.{field}` has no text index; create one with CREATE INDEX ... ON {table} USING gin ({field})"
        )))
    }

    pub fn fts_index_stats(
        &self,
        table_filter: Option<&str>,
    ) -> Result<Vec<FtsIndexStat>, SQLError> {
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        let resolved_filter = match table_filter {
            Some(name) => Some(
                self.try_resolve_table_name(name)
                    .map_err(|err| SQLError::Internal(format!("resolve table filter: {err}")))?
                    .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?,
            ),
            None => None,
        };
        let mut tables: Vec<(String, Arc<TableState>)> = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(name, _)| {
                resolved_filter
                    .as_ref()
                    .is_none_or(|target| name.qualified_name() == *target)
            })
            .map(|(name, table)| (name.qualified_name(), table.clone()))
            .collect();
        tables.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        for (table_name, table) in tables {
            let mut fields = table.fts_fields();
            fields.sort();
            let index = table.inverted_index.read();
            for field in fields {
                let analyzer = self
                    .table_field_analyzer(&table_name, &field)
                    .map_err(SQLError::Internal)?
                    .map_or_else(
                        || analyzer_registry::DEFAULT_ANALYZER_NAME.to_string(),
                        |(name, _)| name,
                    );
                let doc_length_count = index.doc_length_count(Some(&field)).map_err(|error| {
                    SQLError::Internal(format!("read FTS document-length count: {error}"))
                })?;
                out.push(FtsIndexStat {
                    table_name: table_name.clone(),
                    field: field.clone(),
                    analyzer,
                    posting_count: index.posting_count(Some(&field)).map_err(|error| {
                        SQLError::Internal(format!("read FTS posting count: {error}"))
                    })?,
                    doc_length_count,
                    indexed_doc_count: doc_length_count,
                    term_count: index.term_count(Some(&field)).map_err(|error| {
                        SQLError::Internal(format!("read FTS term count: {error}"))
                    })?,
                    total_field_length: index.total_field_length(&field).map_err(|error| {
                        SQLError::Internal(format!("read FTS field length: {error}"))
                    })?,
                });
            }
        }
        Ok(out)
    }

    pub(crate) fn rebuild_fts_index(t: &Arc<TableState>) -> Result<(), String> {
        let fts_fields = t.fts_fields();
        let indexed_docs = {
            let store = t.document_store.read();
            let doc_ids = store.doc_ids().map_err(|error| error.to_string())?;
            let fields: Vec<&str> = fts_fields.iter().map(String::as_str).collect();
            let mut indexed_docs = Vec::with_capacity(doc_ids.len());
            store
                .for_each_fields_multi_ref(&doc_ids, &fields, &mut |doc_id, projected_values| {
                    let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
                    for (field, value) in fts_fields.iter().zip(projected_values) {
                        if let Value::Str(text) = value {
                            text_fields.insert(field.clone(), text.clone());
                        }
                    }
                    if !text_fields.is_empty() {
                        indexed_docs.push((doc_id, text_fields));
                    }
                    true
                })
                .map_err(|error| error.to_string())?;
            indexed_docs
        };
        {
            let mut index = t.inverted_index.write();
            index
                .try_rebuild_documents(indexed_docs)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn add_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| engine.add_document_impl(table, doc_id, document, false),
        )
    }

    pub(crate) fn add_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        mut document: Document,
        known_new: bool,
    ) -> Result<(), SQLError> {
        crate::sql::refresh_stored_generated_columns(self, table, &mut document)?;
        self.add_prepared_document_impl(table, doc_id, document, known_new)
    }

    pub(crate) fn add_prepared_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_impl_with_fts(table, doc_id, document, known_new, true, None)
    }

    pub(crate) fn add_prepared_document_without_fts_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_impl_with_fts(table, doc_id, document, known_new, false, None)
    }

    pub(crate) fn add_prepared_stored_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: uqa_storage::StoredDocument,
        known_new: bool,
    ) -> Result<(), SQLError> {
        let (fields, metadata) = document.into_parts();
        self.add_prepared_document_impl_with_fts(
            table,
            doc_id,
            fields,
            known_new,
            true,
            Some(metadata),
        )
    }

    pub(crate) fn prepared_document_text_fields(
        &self,
        table: &str,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, String>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let mut text_fields = BTreeMap::new();
        for name in &t.fts_fields() {
            if let Some(Value::Str(value)) = document.get(name) {
                text_fields.insert(name.clone(), value.clone());
            }
        }
        Ok(text_fields)
    }

    pub(crate) fn add_prepared_fts_documents(
        &self,
        table: &str,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> Result<(), SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let result = t
            .inverted_index
            .write()
            .try_add_documents(documents)
            .map_err(|error| SQLError::Internal(format!("index documents: {error}")));
        result
    }

    fn add_prepared_document_impl_with_fts(
        &self,
        table: &str,
        doc_id: DocId,
        mut document: Document,
        known_new: bool,
        index_fts: bool,
        metadata: Option<uqa_storage::DocumentMetadata>,
    ) -> Result<(), SQLError> {
        let Some(table_name) = self
            .try_resolve_table_name(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let existed = if known_new {
            false
        } else {
            t.document_store
                .read()
                .get(doc_id)
                .map_err(|error| SQLError::Internal(format!("read existing document: {error}")))?
                .is_some()
        };
        if index_fts {
            let text_fields = self.prepared_document_text_fields(table, &document)?;
            // Replacement is one atomic inverted-index operation even when the new document has no indexed text. Skipping an empty field map would leave stale postings from the previous version; remove-then-add would expose a destructive failure window when analysis fails.
            t.inverted_index
                .write()
                .add_document(doc_id, text_fields)
                .map_err(|error| SQLError::Internal(format!("index document: {error}")))?;
        }
        // Value-index maintenance: unindex the previous field values
        // (put may replace an existing document), index the new ones.
        // `old_indexed` is `None` exactly when no index is built, so
        // the common path costs one read-lock check. A failed put must
        // leave the value indexes untouched. A known-new document has
        // no previous values to unindex, so its writes skip the per-row
        // storage lookup entirely and only insert the new values.
        let (old_indexed, indexed_fields) = if known_new {
            (None, Self::value_indexes_built_fields(&t))
        } else {
            let old = Self::value_indexes_old_values(&t, doc_id)?;
            let fields = old
                .as_ref()
                .map(|old| old.keys().cloned().collect::<Vec<String>>());
            (old, fields)
        };
        let new_indexed: Option<BTreeMap<String, Value>> = indexed_fields.map(|fields| {
            fields
                .into_iter()
                .map(|k| {
                    let value = document.get(&k).cloned().unwrap_or(Value::Null);
                    (k, value)
                })
                .collect()
        });
        let persistent_indexed =
            self.persistent_value_index_document_values(&table_name, &document)?;
        let columns = t.columns.read().clone();
        crate::engine_generated::strip_virtual_generated_columns(&columns, &mut document);
        let metadata = match metadata {
            Some(metadata) => metadata,
            None => uqa_storage::DocumentMetadata::with_tuple_xmin(self.tuple_version_xid()?),
        };
        let mut store = t.document_store.write();
        store
            .put_stored(
                doc_id,
                uqa_storage::StoredDocument::with_metadata(document, metadata),
            )
            .map_err(|err| crate::engine_table_storage::document_store_write_error(&err))?;
        if let Some(new) = persistent_indexed.as_ref() {
            self.persist_value_indexes_apply_write(&table_name, doc_id, Some(new))?;
        }
        if let Some(new) = new_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, old_indexed.as_ref(), Some(new));
        }
        drop(store);
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        // Keep the auto-id watermark monotonic over manual inserts as well.
        let mut nx = t.next_id.lock();
        let next = u128::from(doc_id) + 1;
        if next > *nx {
            *nx = next;
        }
        drop(nx);
        if existed {
            self.note_row_changed(&table_name, doc_id)?;
        } else {
            self.note_row_inserted(&table_name, doc_id)?;
        }
        Ok(())
    }
}
