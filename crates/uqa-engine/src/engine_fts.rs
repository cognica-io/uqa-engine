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
    pub(crate) fn fts_fields_for_table(&self, name: &str) -> Vec<FieldName> {
        self.table(name)
            .map_or_else(Vec::new, |table| table.fts_fields())
    }

    pub fn fts_index_stats(&self, table_filter: Option<&str>) -> Vec<FtsIndexStat> {
        let mut tables: Vec<(String, Arc<TableState>)> = self
            .tables
            .read()
            .iter()
            .filter(|(name, _)| table_filter.is_none_or(|target| name.as_str() == target))
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect();
        tables.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        for (table_name, table) in tables {
            let mut fields = table.fts_fields();
            fields.sort();
            let index = table.inverted_index.read();
            for field in fields {
                let analyzer = self.table_field_analyzer(&table_name, &field).map_or_else(
                    || analyzer_registry::DEFAULT_ANALYZER_NAME.to_string(),
                    |(name, _)| name,
                );
                let doc_length_count = index.doc_length_count(Some(&field));
                out.push(FtsIndexStat {
                    table_name: table_name.clone(),
                    field: field.clone(),
                    analyzer,
                    posting_count: index.posting_count(Some(&field)),
                    doc_length_count,
                    indexed_doc_count: doc_length_count,
                    term_count: index.term_count(Some(&field)),
                    total_field_length: index.total_field_length(&field),
                });
            }
        }
        out
    }

    pub(crate) fn rebuild_fts_index(t: &Arc<TableState>) -> Result<(), String> {
        let fts_fields = t.fts_fields();
        let indexed_docs = {
            let store = t.document_store.read();
            let doc_ids = store.doc_ids();
            let fields: Vec<&str> = fts_fields.iter().map(String::as_str).collect();
            let mut indexed_docs = Vec::with_capacity(doc_ids.len());
            store.for_each_fields_multi_ref(&doc_ids, &fields, &mut |doc_id, projected_values| {
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
            });
            indexed_docs
        };
        {
            let mut index = t.inverted_index.write();
            index.try_rebuild_documents(indexed_docs)?;
        }
        Ok(())
    }

    pub fn add_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.add_document_impl(table, doc_id, document, false)
    }

    /// [`Engine::add_document`] for a document id the caller has proven
    /// absent (a fresh id from the allocator, or an INSERT whose
    /// uniqueness validation already ran). Skips the per-row old-value
    /// lookup that value-index maintenance needs for replacements.
    pub(crate) fn add_document_known_new(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.add_document_impl(table, doc_id, document, true)
    }

    fn add_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        known_new: bool,
    ) -> Result<(), SQLError> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(());
        };
        let Some(t) = self.table(table) else {
            return Ok(());
        };
        // Index the FTS fields whose values are strings.
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for name in &t.fts_fields() {
            if let Some(Value::Str(s)) = document.get(name) {
                text_fields.insert(name.clone(), s.clone());
            }
        }
        if !text_fields.is_empty() {
            t.inverted_index.write().add_document(doc_id, text_fields);
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
            let old = Self::value_indexes_old_values(&t, doc_id);
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
            self.persistent_value_index_document_values(&table_name, &document);
        let mut store = t.document_store.write();
        store
            .put(doc_id, document)
            .map_err(|err| crate::engine_table_storage::document_store_write_error(&err))?;
        if let Some(new) = persistent_indexed.as_ref() {
            self.persist_value_indexes_apply_write(&table_name, doc_id, Some(new))?;
        }
        if let Some(new) = new_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, old_indexed.as_ref(), Some(new));
        }
        drop(store);
        self.mark_column_stats_dirty(&table_name, &t);
        // Keep the auto-id watermark monotonic over manual inserts as well.
        let mut nx = t.next_id.lock();
        if doc_id >= *nx {
            *nx = doc_id + 1;
        }
        Ok(())
    }
}
