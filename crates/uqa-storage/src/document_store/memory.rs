//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory document storage and borrowed projection scans.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, Value};

use crate::backend::StorageBackendResult;

use super::{Document, DocumentStore};

mod projection;

#[derive(Debug, Default, Clone)]
pub struct MemoryDocumentStore {
    documents: BTreeMap<DocId, Document>,
    document_layout_ids: BTreeMap<DocId, usize>,
    layouts: Vec<Vec<String>>,
}

impl MemoryDocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&DocId, &Document)> {
        self.documents.iter()
    }
}

fn intern_document_layout(layouts: &mut Vec<Vec<String>>, document: &Document) -> usize {
    if let Some(layout_id) = layouts.iter().position(|layout| {
        layout.len() == document.len()
            && layout
                .iter()
                .map(String::as_str)
                .eq(document.keys().map(String::as_str))
    }) {
        return layout_id;
    }
    layouts.push(document.keys().cloned().collect());
    layouts.len() - 1
}

impl DocumentStore for MemoryDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        let layout_id = intern_document_layout(&mut self.layouts, &document);
        self.documents.insert(doc_id, document);
        self.document_layout_ids.insert(doc_id, layout_id);
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        Ok(self.documents.get(&doc_id).cloned())
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.documents.contains_key(&doc_id))
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        Ok(self
            .documents
            .get(&doc_id)
            .and_then(|d| d.get(field).cloned()))
    }

    fn get_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        Ok(doc_ids
            .iter()
            .filter_map(|doc_id| {
                let document = self.documents.get(doc_id)?;
                let values = fields
                    .iter()
                    .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                    .collect();
                Some((*doc_id, values))
            })
            .collect())
    }

    fn for_each_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) -> StorageBackendResult<()> {
        for doc_id in doc_ids {
            let values = self.documents.get(doc_id).map_or_else(
                || vec![Value::Null; fields.len()],
                |document| {
                    fields
                        .iter()
                        .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                        .collect()
                },
            );
            if !visitor(*doc_id, values) {
                break;
            }
        }
        Ok(())
    }

    fn for_each_fields_multi_ref(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit_fields_multi_ref_with_presence(doc_ids, fields, &mut |doc_id, _, values| {
            visitor(doc_id, values)
        });
        Ok(())
    }

    fn for_each_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit_fields_multi_ref_with_presence(doc_ids, fields, visitor);
        Ok(())
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        Ok(self
            .documents
            .iter()
            .find_map(|(doc_id, doc)| (doc.get(field) == Some(value)).then_some(*doc_id)))
    }

    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        Ok(self.documents.iter().find_map(|(doc_id, doc)| {
            fields
                .iter()
                .zip(values.iter())
                .all(|(field, value)| doc.get(field).unwrap_or(&Value::Null) == value)
                .then_some(*doc_id)
        }))
    }

    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        let layout_id = {
            let Some(document) = self.documents.get_mut(&doc_id) else {
                return Ok(false);
            };
            for (field, value) in updates {
                if matches!(value, Value::Null) {
                    document.remove(field);
                } else {
                    document.insert(field.clone(), value.clone());
                }
            }
            intern_document_layout(&mut self.layouts, document)
        };
        self.document_layout_ids.insert(doc_id, layout_id);
        Ok(true)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.documents.remove(&doc_id);
        self.document_layout_ids.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.documents.clear();
        self.document_layout_ids.clear();
        self.layouts.clear();
        Ok(())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(self.documents.keys().copied().collect())
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        use std::ops::Bound::{Excluded, Unbounded};

        Ok(match after {
            Some(after) => self
                .documents
                .range((Excluded(after), Unbounded))
                .next()
                .map(|(doc_id, _)| *doc_id),
            None => self.documents.keys().next().copied(),
        })
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        use std::ops::Bound::{Excluded, Unbounded};

        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(match after {
            Some(after) => self
                .documents
                .range((Excluded(after), Unbounded))
                .take(limit)
                .map(|(doc_id, _)| *doc_id)
                .collect(),
            None => self.documents.keys().take(limit).copied().collect(),
        })
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.documents.len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        Ok(Box::new(self.clone()))
    }
}

#[cfg(test)]
mod tests;
