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

use super::{Document, DocumentMetadata, DocumentStore, SharedDocumentRow, StoredDocument};

mod projection;

#[derive(Debug, Default, Clone)]
pub struct MemoryDocumentStore {
    documents: BTreeMap<DocId, MemoryDocumentRow>,
    layouts: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(super) struct MemoryDocumentRow {
    layout_id: usize,
    values: Arc<Vec<Value>>,
    metadata: DocumentMetadata,
}

impl MemoryDocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (DocId, Document)> + '_ {
        self.documents
            .iter()
            .map(|(doc_id, stored)| (*doc_id, self.materialize_document(stored)))
    }

    fn materialize_document(&self, stored: &MemoryDocumentRow) -> Document {
        self.layouts[stored.layout_id]
            .iter()
            .cloned()
            .zip(stored.values.iter().cloned())
            .collect()
    }

    fn materialize_stored_document(&self, stored: &MemoryDocumentRow) -> StoredDocument {
        StoredDocument::with_metadata(self.materialize_document(stored), stored.metadata)
    }

    fn field<'a>(&'a self, stored: &'a MemoryDocumentRow, field: &str) -> Option<&'a Value> {
        let slot = self.layouts[stored.layout_id]
            .binary_search_by(|stored| stored.as_str().cmp(field))
            .ok()?;
        stored.values.get(slot)
    }

    fn put_stored_inner(&mut self, doc_id: DocId, document: StoredDocument) {
        let (document, metadata) = document.into_parts();
        let (layout_id, values) =
            if let Some(layout_id) = document_layout_id(&self.layouts, &document) {
                (layout_id, document.into_values().collect())
            } else {
                let (layout, values): (Vec<_>, Vec<_>) = document.into_iter().unzip();
                let layout_id = self.layouts.len();
                self.layouts.push(layout);
                (layout_id, values)
            };
        self.documents.insert(
            doc_id,
            MemoryDocumentRow {
                layout_id,
                values: Arc::new(values),
                metadata,
            },
        );
    }
}

fn document_layout_id(layouts: &[Vec<String>], document: &Document) -> Option<usize> {
    layouts.iter().position(|layout| {
        layout.len() == document.len()
            && layout
                .iter()
                .map(String::as_str)
                .eq(document.keys().map(String::as_str))
    })
}

impl DocumentStore for MemoryDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        let metadata = self
            .documents
            .get(&doc_id)
            .map_or_else(DocumentMetadata::default, |stored| stored.metadata);
        self.put_stored_inner(doc_id, StoredDocument::with_metadata(document, metadata));
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        Ok(self
            .documents
            .get(&doc_id)
            .map(|stored| self.materialize_document(stored)))
    }

    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.put_stored_inner(doc_id, document);
        Ok(())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(self
            .documents
            .get(&doc_id)
            .map(|stored| self.materialize_stored_document(stored)))
    }

    fn get_stored_many(
        &self,
        doc_ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        Ok(doc_ids
            .iter()
            .filter_map(|doc_id| {
                self.documents
                    .get(doc_id)
                    .map(|stored| (*doc_id, self.materialize_stored_document(stored)))
            })
            .collect())
    }

    fn get_metadata(&self, doc_id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        Ok(self.documents.get(&doc_id).map(|stored| stored.metadata))
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.documents.contains_key(&doc_id))
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        Ok(self
            .documents
            .get(&doc_id)
            .and_then(|stored| self.field(stored, field).cloned()))
    }

    fn get_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        Ok(doc_ids
            .iter()
            .filter_map(|doc_id| {
                let stored = self.documents.get(doc_id)?;
                let values = fields
                    .iter()
                    .map(|field| self.field(stored, field).cloned().unwrap_or(Value::Null))
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
                |stored| {
                    fields
                        .iter()
                        .map(|field| self.field(stored, field).cloned().unwrap_or(Value::Null))
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

    fn get_shared_fields(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<SharedDocumentRow>>>> {
        Ok(Some(self.shared_fields(doc_ids, fields)))
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        Ok(self.documents.iter().find_map(|(doc_id, stored)| {
            (self.field(stored, field) == Some(value)).then_some(*doc_id)
        }))
    }

    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        Ok(self.documents.iter().find_map(|(doc_id, stored)| {
            fields
                .iter()
                .zip(values.iter())
                .all(|(field, value)| self.field(stored, field).unwrap_or(&Value::Null) == value)
                .then_some(*doc_id)
        }))
    }

    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        let Some(mut document) = self
            .documents
            .get(&doc_id)
            .map(|stored| self.materialize_stored_document(stored))
        else {
            return Ok(false);
        };
        for (field, value) in updates {
            if matches!(value, Value::Null) {
                document.fields_mut().remove(field);
            } else {
                document.fields_mut().insert(field.clone(), value.clone());
            }
        }
        self.put_stored(doc_id, document)?;
        Ok(true)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.documents.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.documents.clear();
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

    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, SharedDocumentRow)>>> {
        Ok(Some(self.next_shared_rows(after, limit, fields)))
    }

    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        Ok(Some(self.visit_next_rows(after, limit, fields, visitor)))
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
