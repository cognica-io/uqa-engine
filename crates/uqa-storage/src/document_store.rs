//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document storage abstraction.
//!
//! A `DocumentStore` maps [`DocId`] keys to field maps and supports
//! field-level access. Phase 1 ships only [`MemoryDocumentStore`];
//! `SQLite`-backed implementations land alongside the catalog.

use std::collections::BTreeMap;

use uqa_core::{DocId, FieldName, Value};

/// Document field map. Keys are field names; values are dynamic.
pub type Document = BTreeMap<FieldName, Value>;

pub trait DocumentStore: Send + Sync {
    fn put(&mut self, doc_id: DocId, document: Document);
    fn get(&self, doc_id: DocId) -> Option<&Document>;
    fn delete(&mut self, doc_id: DocId);
    fn clear(&mut self);

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<&Value> {
        self.get(doc_id).and_then(|d| d.get(field))
    }

    fn doc_ids(&self) -> Vec<DocId>;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryDocumentStore {
    documents: BTreeMap<DocId, Document>,
}

impl MemoryDocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&DocId, &Document)> {
        self.documents.iter()
    }
}

impl DocumentStore for MemoryDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) {
        self.documents.insert(doc_id, document);
    }

    fn get(&self, doc_id: DocId) -> Option<&Document> {
        self.documents.get(&doc_id)
    }

    fn delete(&mut self, doc_id: DocId) {
        self.documents.remove(&doc_id);
    }

    fn clear(&mut self) {
        self.documents.clear();
    }

    fn doc_ids(&self) -> Vec<DocId> {
        self.documents.keys().copied().collect()
    }

    fn len(&self) -> usize {
        self.documents.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn put_get_round_trip() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("title", Value::Str("rust".into()))]));
        let got = s.get(1).unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
    }

    #[test]
    fn get_field_returns_value() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))]));
        assert_eq!(s.get_field(1, "year"), Some(&Value::Int(2026)));
        assert_eq!(s.get_field(1, "missing"), None);
        assert_eq!(s.get_field(99, "year"), None);
    }

    #[test]
    fn delete_removes_doc() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("a", Value::Int(1))]));
        s.delete(1);
        assert!(s.get(1).is_none());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn doc_ids_returns_all() {
        let mut s = MemoryDocumentStore::new();
        s.put(2, Document::new());
        s.put(1, Document::new());
        s.put(3, Document::new());
        assert_eq!(s.doc_ids(), vec![1, 2, 3]);
    }
}
