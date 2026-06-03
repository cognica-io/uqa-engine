//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, Arc, BTreeMap, DocId, Document, Engine, FieldName, FtsIndexStat, TableState,
    Value,
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
        let docs: Vec<(DocId, Document)> = {
            let store = t.document_store.read();
            store.iter_all().collect()
        };
        let mut index = t.inverted_index.write();
        index.try_clear()?;
        for (doc_id, document) in docs {
            let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
            for field in &fts_fields {
                if let Some(Value::Str(s)) = document.get(field) {
                    text_fields.insert(field.clone(), s.clone());
                }
            }
            if !text_fields.is_empty() {
                index.try_add_document(doc_id, text_fields)?;
            }
        }
        Ok(())
    }

    pub fn add_document(&self, table: &str, doc_id: DocId, document: Document) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
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
        t.document_store.write().put(doc_id, document);
        self.mark_column_stats_dirty(&table_name, &t);
        // Keep the auto-id watermark monotonic over manual inserts as well.
        let mut nx = t.next_id.lock();
        if doc_id >= *nx {
            *nx = doc_id + 1;
        }
    }
}
