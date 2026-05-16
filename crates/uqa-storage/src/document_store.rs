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
use std::sync::Arc;

use uqa_core::{DocId, FieldName, PathSegment, Value};

/// Document field map. Keys are field names; values are dynamic.
pub type Document = BTreeMap<FieldName, Value>;

pub trait DocumentStore: Send + Sync {
    fn put(&mut self, doc_id: DocId, document: Document);
    fn get(&self, doc_id: DocId) -> Option<Document>;
    fn delete(&mut self, doc_id: DocId);
    fn clear(&mut self);

    /// Read a single field. Returns an owned [`Value`] so persistent
    /// backends (`SQLite`, ...) can decode on demand without reaching
    /// for a reference into a transient row.
    fn get_field(&self, doc_id: DocId, field: &str) -> Option<Value> {
        self.get(doc_id).and_then(|d| d.get(field).cloned())
    }

    /// Bulk variant of [`DocumentStore::get_field`]. The default
    /// implementation walks each id one at a time; persistent backends
    /// should override to run a single batched query.
    fn get_fields_bulk(&self, doc_ids: &[DocId], field: &str) -> BTreeMap<DocId, Value> {
        doc_ids
            .iter()
            .map(|id| (*id, self.get_field(*id, field).unwrap_or(Value::Null)))
            .collect()
    }

    /// Return `true` if any document has `field == value`.
    fn has_value(&self, field: &str, value: &Value) -> bool {
        self.doc_ids()
            .into_iter()
            .any(|id| self.get_field(id, field).as_ref() == Some(value))
    }

    /// Evaluate a hierarchical path expression against a document.
    /// Mirrors Python `DocumentStore.eval_path` /
    /// `HierarchicalDocument.eval_path` semantics.
    fn eval_path(&self, doc_id: DocId, path: &[PathSegment]) -> Option<Value> {
        let doc = self.get(doc_id)?;
        eval_path_in_document(&doc, path)
    }

    fn doc_ids(&self) -> Vec<DocId>;

    fn max_doc_id(&self) -> DocId {
        self.doc_ids().into_iter().max().unwrap_or(0)
    }

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over `(doc_id, document)` pairs in id order. The default
    /// implementation fetches each document individually; SQLite-backed
    /// stores override with a single query.
    fn iter_all(&self) -> Box<dyn Iterator<Item = (DocId, Document)> + '_> {
        let mut ids = self.doc_ids();
        ids.sort_unstable();
        let snapshot = self.snapshot();
        Box::new(
            ids.into_iter()
                .filter_map(move |id| snapshot.get(id).map(|doc| (id, doc))),
        )
    }

    /// Read-only handle suitable for an `ExecutionContext`. Persistent
    /// backends share their connection; memory backends deep-clone so the
    /// snapshot is isolated from later mutations.
    fn snapshot(&self) -> Arc<dyn DocumentStore>;
}

/// Walk a document along a [`PathSegment`] sequence. Mirrors Python
/// `HierarchicalDocument.eval_path` — descending strings into maps,
/// integers into lists, with the implicit array-wildcard rule that
/// applies a string component over every map element of an array.
pub fn eval_path_in_document(doc: &Document, path: &[PathSegment]) -> Option<Value> {
    let mut current: Value = match path.first()? {
        PathSegment::Key(k) => doc.get(k)?.clone(),
        PathSegment::Index(_) => return None,
    };
    for seg in path.iter().skip(1) {
        current = match (current, seg) {
            (Value::Map(m), PathSegment::Key(k)) => m.get(k)?.clone(),
            (Value::List(items), PathSegment::Index(i)) => items.get(*i)?.clone(),
            (Value::List(items), PathSegment::Key(k)) => {
                let collected: Vec<Value> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::Map(m) => m.get(k).cloned(),
                        _ => None,
                    })
                    .collect();
                Value::List(collected)
            }
            _ => return None,
        };
    }
    Some(current)
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

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.documents.get(&doc_id).cloned()
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<Value> {
        self.documents
            .get(&doc_id)
            .and_then(|d| d.get(field).cloned())
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

    fn snapshot(&self) -> Arc<dyn DocumentStore> {
        Arc::new(self.clone())
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
        assert_eq!(s.get_field(1, "year"), Some(Value::Int(2026)));
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

    #[test]
    fn get_fields_bulk_returns_value_per_id_with_null_for_missing() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))]));
        s.put(2, doc([("year", Value::Int(2025))]));
        let got = s.get_fields_bulk(&[1, 2, 99], "year");
        assert_eq!(got.get(&1), Some(&Value::Int(2026)));
        assert_eq!(got.get(&2), Some(&Value::Int(2025)));
        assert_eq!(got.get(&99), Some(&Value::Null));
    }

    #[test]
    fn has_value_returns_true_when_any_doc_matches() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("color", Value::Str("red".into()))]));
        s.put(2, doc([("color", Value::Str("blue".into()))]));
        assert!(s.has_value("color", &Value::Str("red".into())));
        assert!(!s.has_value("color", &Value::Str("green".into())));
    }

    #[test]
    fn eval_path_walks_nested_map() {
        let mut s = MemoryDocumentStore::new();
        let mut nested = BTreeMap::new();
        nested.insert("name".to_string(), Value::Str("alice".into()));
        s.put(1, doc([("user", Value::Map(nested))]));
        let path = vec![
            uqa_core::PathSegment::Key("user".into()),
            uqa_core::PathSegment::Key("name".into()),
        ];
        assert_eq!(s.eval_path(1, &path), Some(Value::Str("alice".into())));
    }

    #[test]
    fn iter_all_yields_in_id_order() {
        let mut s = MemoryDocumentStore::new();
        s.put(3, doc([("k", Value::Int(3))]));
        s.put(1, doc([("k", Value::Int(1))]));
        s.put(2, doc([("k", Value::Int(2))]));
        let collected: Vec<u64> = s.iter_all().map(|(id, _)| id).collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }
}
