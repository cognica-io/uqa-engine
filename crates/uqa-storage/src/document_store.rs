//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document storage abstraction.
//!
//! A `DocumentStore` maps [`DocId`] keys to field maps and supports
//! field-level access. The storage crate provides in-memory, `SQLite`,
//! and Key/Value-backed implementations behind the same trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, FieldName, PathSegment, Value};

use crate::backend::StorageBackendResult;

/// Document field map. Keys are field names; values are dynamic.
pub type Document = BTreeMap<FieldName, Value>;

/// Mutating methods are fallible: persistent backends surface their
/// write failures so callers (engine DML, upserts, referential
/// rewrites) can abort the enclosing transaction instead of silently
/// committing a partially-applied statement. A rewrite that deletes a
/// row and then fails to re-insert it must never look like success.
pub trait DocumentStore: Send + Sync {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()>;
    fn get(&self, doc_id: DocId) -> Option<Document>;
    fn contains_doc_id(&self, doc_id: DocId) -> bool {
        self.get(doc_id).is_some()
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()>;
    fn clear(&mut self) -> StorageBackendResult<()>;

    /// Read a single field. Returns an owned [`Value`] so persistent
    /// backends (`SQLite`, ...) can decode on demand without reaching
    /// for a reference into a transient row.
    fn get_field(&self, doc_id: DocId, field: &str) -> Option<Value> {
        self.get(doc_id).and_then(|d| d.get(field).cloned())
    }

    /// Find the first document whose top-level field equals `value`.
    /// Persistent stores can override this with an indexed or JSON-path
    /// lookup so point updates do not have to materialise every row.
    fn find_doc_id_by_field(&self, field: &str, value: &Value) -> Option<DocId> {
        self.doc_ids()
            .into_iter()
            .find(|id| self.get_field(*id, field).as_ref() == Some(value))
    }

    /// Apply top-level field updates without requiring callers to
    /// materialise the whole document. `Value::Null` matches `put` by
    /// removing the stored field. `Ok(false)` means the document does
    /// not exist; write failures surface as `Err`.
    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        let Some(mut document) = self.get(doc_id) else {
            return Ok(false);
        };
        for (field, value) in updates {
            if matches!(value, Value::Null) {
                document.remove(field);
            } else {
                document.insert(field.clone(), value.clone());
            }
        }
        self.put(doc_id, document)?;
        Ok(true)
    }

    /// Bulk variant of [`DocumentStore::get`]. Ids without a stored
    /// document are absent from the result. The default implementation
    /// walks each id one at a time; persistent backends should
    /// override to batch the reads into few queries.
    fn get_many(&self, doc_ids: &[DocId]) -> BTreeMap<DocId, Document> {
        doc_ids
            .iter()
            .filter_map(|id| self.get(*id).map(|document| (*id, document)))
            .collect()
    }

    /// Fetch several top-level fields for many documents. The result
    /// vector is aligned with `fields`; missing fields come back as
    /// [`Value::Null`], ids without a document are absent. Persistent
    /// backends override this to extract all fields in one scan
    /// instead of materialising whole documents.
    fn get_fields_multi(&self, doc_ids: &[DocId], fields: &[&str]) -> BTreeMap<DocId, Vec<Value>> {
        doc_ids
            .iter()
            .filter_map(|id| {
                let document = self.get(*id)?;
                let values = fields
                    .iter()
                    .map(|f| document.get(*f).cloned().unwrap_or(Value::Null))
                    .collect();
                Some((*id, values))
            })
            .collect()
    }

    /// Visit a column projection in the caller's document-id order.
    /// The callback receives one owned row at a time, allowing scan and
    /// aggregate pipelines to avoid materialising a second doc-id map.
    /// Returning `false` stops the visit early. Missing documents yield
    /// a row of NULLs, matching row-evaluator semantics.
    fn for_each_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) {
        let mut projected = self.get_fields_multi(doc_ids, fields);
        for doc_id in doc_ids {
            let values = projected
                .remove(doc_id)
                .unwrap_or_else(|| vec![Value::Null; fields.len()]);
            if !visitor(*doc_id, values) {
                break;
            }
        }
    }

    /// Visit a column projection by reference when the backend can keep
    /// decoded values alive for the duration of the callback. The default
    /// adapter preserves the backend's owned/batched projection path;
    /// in-memory stores override it to avoid cloning every projected value.
    fn for_each_fields_multi_ref(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) {
        self.for_each_fields_multi(doc_ids, fields, &mut |doc_id, values| {
            let references: Vec<&Value> = values.iter().collect();
            visitor(doc_id, &references)
        });
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

    /// Find the first document whose top-level fields match every
    /// requested value.
    fn find_doc_id_by_fields(&self, fields: &[String], values: &[Value]) -> Option<DocId> {
        if fields.is_empty() || fields.len() != values.len() {
            return None;
        }
        self.doc_ids().into_iter().find(|id| {
            fields
                .iter()
                .zip(values.iter())
                .all(|(field, value)| self.get_field(*id, field).unwrap_or(Value::Null) == *value)
        })
    }

    /// Evaluate a hierarchical path expression against a document.
    /// Matches UQA behavior for `DocumentStore.eval_path` /
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

/// Walk a document along a [`PathSegment`] sequence. Matches UQA behavior for
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

fn project_document_fields_ref<'a>(
    document: Option<&'a Document>,
    fields: &[&str],
    fields_are_sorted: bool,
    null: &'a Value,
    values: &mut Vec<&'a Value>,
) {
    values.clear();
    match document {
        Some(document) if fields_are_sorted => {
            // Both sides are ordered. Merge them once instead of
            // performing a B-tree lookup for every projected field.
            let mut stored_fields = document.iter();
            let mut current = stored_fields.next();
            for requested in fields {
                while current.is_some_and(|(stored, _)| stored.as_str() < *requested) {
                    current = stored_fields.next();
                }
                values.push(match current {
                    Some((stored, value)) if stored.as_str() == *requested => value,
                    _ => null,
                });
            }
        }
        Some(document) => values.extend(
            fields
                .iter()
                .map(|field| document.get(*field).unwrap_or(null)),
        ),
        None => values.resize(fields.len(), null),
    }
}

impl DocumentStore for MemoryDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        self.documents.insert(doc_id, document);
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.documents.get(&doc_id).cloned()
    }

    fn contains_doc_id(&self, doc_id: DocId) -> bool {
        self.documents.contains_key(&doc_id)
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<Value> {
        self.documents
            .get(&doc_id)
            .and_then(|d| d.get(field).cloned())
    }

    fn get_fields_multi(&self, doc_ids: &[DocId], fields: &[&str]) -> BTreeMap<DocId, Vec<Value>> {
        doc_ids
            .iter()
            .filter_map(|doc_id| {
                let document = self.documents.get(doc_id)?;
                let values = fields
                    .iter()
                    .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                    .collect();
                Some((*doc_id, values))
            })
            .collect()
    }

    fn for_each_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) {
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
    }

    fn for_each_fields_multi_ref(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) {
        let null = Value::Null;
        let mut values = Vec::with_capacity(fields.len());
        let fields_are_sorted = fields.windows(2).all(|pair| pair[0] <= pair[1]);

        let doc_ids_are_sorted = doc_ids.windows(2).all(|pair| pair[0] <= pair[1]);
        let use_merge_scan = doc_ids_are_sorted
            && doc_ids.len().saturating_mul(8) >= self.documents.len()
            && !doc_ids.is_empty();
        if use_merge_scan {
            // Posting-list output is ordered and analytical predicates are
            // commonly dense. Walk the document tree once in that case;
            // sparse probes retain logarithmic point lookup below.
            let mut documents = self.documents.range(doc_ids[0]..);
            let mut current = documents.next();
            for doc_id in doc_ids {
                while current.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                    current = documents.next();
                }
                let document = match current {
                    Some((stored_id, document)) if stored_id == doc_id => Some(document),
                    _ => None,
                };
                project_document_fields_ref(
                    document,
                    fields,
                    fields_are_sorted,
                    &null,
                    &mut values,
                );
                if !visitor(*doc_id, &values) {
                    return;
                }
            }
            return;
        }

        for doc_id in doc_ids {
            project_document_fields_ref(
                self.documents.get(doc_id),
                fields,
                fields_are_sorted,
                &null,
                &mut values,
            );
            if !visitor(*doc_id, &values) {
                return;
            }
        }
    }

    fn find_doc_id_by_field(&self, field: &str, value: &Value) -> Option<DocId> {
        self.documents
            .iter()
            .find_map(|(doc_id, doc)| (doc.get(field) == Some(value)).then_some(*doc_id))
    }

    fn find_doc_id_by_fields(&self, fields: &[String], values: &[Value]) -> Option<DocId> {
        if fields.is_empty() || fields.len() != values.len() {
            return None;
        }
        self.documents.iter().find_map(|(doc_id, doc)| {
            fields
                .iter()
                .zip(values.iter())
                .all(|(field, value)| doc.get(field).unwrap_or(&Value::Null) == value)
                .then_some(*doc_id)
        })
    }

    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
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
        Ok(true)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.documents.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.documents.clear();
        Ok(())
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
        s.put(1, doc([("title", Value::Str("rust".into()))]))
            .unwrap();
        let got = s.get(1).unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
    }

    #[test]
    fn get_field_returns_value() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))])).unwrap();
        assert_eq!(s.get_field(1, "year"), Some(Value::Int(2026)));
        assert_eq!(s.get_field(1, "missing"), None);
        assert_eq!(s.get_field(99, "year"), None);
    }

    #[test]
    fn delete_removes_doc() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("a", Value::Int(1))])).unwrap();
        s.delete(1).unwrap();
        assert!(s.get(1).is_none());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn doc_ids_returns_all() {
        let mut s = MemoryDocumentStore::new();
        s.put(2, Document::new()).unwrap();
        s.put(1, Document::new()).unwrap();
        s.put(3, Document::new()).unwrap();
        assert_eq!(s.doc_ids(), vec![1, 2, 3]);
    }

    #[test]
    fn get_fields_bulk_returns_value_per_id_with_null_for_missing() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))])).unwrap();
        s.put(2, doc([("year", Value::Int(2025))])).unwrap();
        let got = s.get_fields_bulk(&[1, 2, 99], "year");
        assert_eq!(got.get(&1), Some(&Value::Int(2026)));
        assert_eq!(got.get(&2), Some(&Value::Int(2025)));
        assert_eq!(got.get(&99), Some(&Value::Null));
    }

    #[test]
    fn get_fields_multi_projects_in_requested_order() {
        let mut s = MemoryDocumentStore::new();
        s.put(
            1,
            doc([
                ("year", Value::Int(2026)),
                ("title", Value::Str("rust".into())),
                ("unused", Value::Bool(true)),
            ]),
        )
        .unwrap();

        let got = s.get_fields_multi(&[1, 99], &["title", "missing", "year"]);
        assert_eq!(
            got.get(&1),
            Some(&vec![
                Value::Str("rust".into()),
                Value::Null,
                Value::Int(2026)
            ])
        );
        assert!(!got.contains_key(&99));
    }

    #[test]
    fn for_each_fields_multi_streams_doc_id_order_and_can_stop() {
        let mut s = MemoryDocumentStore::new();
        s.put(2, doc([("value", Value::Int(20))])).unwrap();
        s.put(1, doc([("value", Value::Int(10))])).unwrap();

        let mut visited = Vec::new();
        s.for_each_fields_multi(&[2, 99, 1], &["value"], &mut |doc_id, values| {
            visited.push((doc_id, values));
            doc_id != 99
        });

        assert_eq!(
            visited,
            vec![(2, vec![Value::Int(20)]), (99, vec![Value::Null])]
        );
    }

    #[test]
    fn for_each_fields_multi_ref_borrows_memory_values_and_reuses_nulls() {
        let mut s = MemoryDocumentStore::new();
        s.put(2, doc([("value", Value::Str("twenty".into()))]))
            .unwrap();
        let stored = std::ptr::from_ref(s.documents[&2].get("value").unwrap());

        let mut visited = Vec::new();
        s.for_each_fields_multi_ref(&[2, 99], &["value"], &mut |doc_id, values| {
            if doc_id == 2 {
                assert_eq!(std::ptr::from_ref(values[0]), stored);
            }
            visited.push((doc_id, values[0].clone()));
            true
        });

        assert_eq!(
            visited,
            vec![(2, Value::Str("twenty".into())), (99, Value::Null),]
        );
    }

    #[test]
    fn for_each_fields_multi_ref_preserves_projection_order_across_scan_paths() {
        let mut s = MemoryDocumentStore::new();
        for doc_id in 1..=10 {
            s.put(
                doc_id,
                doc([
                    ("alpha", Value::Int(doc_id as i64)),
                    ("middle", Value::Bool(true)),
                    ("zulu", Value::Int((doc_id * 10) as i64)),
                ]),
            )
            .unwrap();
        }

        let mut dense = Vec::new();
        s.for_each_fields_multi_ref(
            &[2, 3, 4, 99],
            &["alpha", "missing", "zulu"],
            &mut |doc_id, values| {
                dense.push((
                    doc_id,
                    values.iter().map(|value| (*value).clone()).collect(),
                ));
                true
            },
        );
        assert_eq!(
            dense,
            vec![
                (2, vec![Value::Int(2), Value::Null, Value::Int(20)]),
                (3, vec![Value::Int(3), Value::Null, Value::Int(30)]),
                (4, vec![Value::Int(4), Value::Null, Value::Int(40)]),
                (99, vec![Value::Null, Value::Null, Value::Null]),
            ]
        );

        let mut unsorted = Vec::new();
        s.for_each_fields_multi_ref(&[10, 1], &["zulu", "alpha"], &mut |doc_id, values| {
            unsorted.push((
                doc_id,
                values.iter().map(|value| (*value).clone()).collect(),
            ));
            true
        });
        assert_eq!(
            unsorted,
            vec![
                (10, vec![Value::Int(100), Value::Int(10)]),
                (1, vec![Value::Int(10), Value::Int(1)]),
            ]
        );
    }

    #[test]
    fn has_value_returns_true_when_any_doc_matches() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("color", Value::Str("red".into()))]))
            .unwrap();
        s.put(2, doc([("color", Value::Str("blue".into()))]))
            .unwrap();
        assert!(s.has_value("color", &Value::Str("red".into())));
        assert!(!s.has_value("color", &Value::Str("green".into())));
    }

    #[test]
    fn find_doc_id_by_field_returns_first_match() {
        let mut s = MemoryDocumentStore::new();
        s.put(3, doc([("public_id", Value::Str("m-3".into()))]))
            .unwrap();
        s.put(7, doc([("public_id", Value::Str("m-7".into()))]))
            .unwrap();

        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("m-7".into())),
            Some(7)
        );
        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("missing".into())),
            None
        );
    }

    #[test]
    fn patch_fields_updates_and_removes_top_level_values() {
        let mut s = MemoryDocumentStore::new();
        s.put(
            1,
            doc([
                ("public_id", Value::Str("m-1".into())),
                ("content", Value::Str("old".into())),
                ("token_count", Value::Int(4)),
            ]),
        )
        .unwrap();

        let updates = BTreeMap::from([
            ("content".to_string(), Value::Str("new".into())),
            ("token_count".to_string(), Value::Null),
        ]);
        assert!(s.patch_fields(1, &updates).unwrap());

        let got = s.get(1).unwrap();
        assert_eq!(got.get("public_id"), Some(&Value::Str("m-1".into())));
        assert_eq!(got.get("content"), Some(&Value::Str("new".into())));
        assert!(!got.contains_key("token_count"));
    }

    #[test]
    fn eval_path_walks_nested_map() {
        let mut s = MemoryDocumentStore::new();
        let mut nested = BTreeMap::new();
        nested.insert("name".to_string(), Value::Str("alice".into()));
        s.put(1, doc([("user", Value::Map(nested))])).unwrap();
        let path = vec![
            uqa_core::PathSegment::Key("user".into()),
            uqa_core::PathSegment::Key("name".into()),
        ];
        assert_eq!(s.eval_path(1, &path), Some(Value::Str("alice".into())));
    }

    #[test]
    fn iter_all_yields_in_id_order() {
        let mut s = MemoryDocumentStore::new();
        s.put(3, doc([("k", Value::Int(3))])).unwrap();
        s.put(1, doc([("k", Value::Int(1))])).unwrap();
        s.put(2, doc([("k", Value::Int(2))])).unwrap();
        let collected: Vec<u64> = s.iter_all().map(|(id, _)| id).collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }
}
