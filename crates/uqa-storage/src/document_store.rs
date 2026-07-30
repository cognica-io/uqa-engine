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

use crate::backend::{StorageBackendError, StorageBackendResult};

/// Document field map. Keys are field names; values are dynamic.
pub type Document = BTreeMap<FieldName, Value>;

/// Mutating methods are fallible: persistent backends surface their
/// write failures so callers (engine DML, upserts, referential
/// rewrites) can abort the enclosing transaction instead of silently
/// committing a partially-applied statement. A rewrite that deletes a
/// row and then fails to re-insert it must never look like success.
pub trait DocumentStore: Send + Sync {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()>;
    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>>;
    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.get(doc_id)?.is_some())
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()>;
    fn clear(&mut self) -> StorageBackendResult<()>;

    /// Read a single field. Returns an owned [`Value`] so persistent
    /// backends (`SQLite`, ...) can decode on demand without reaching
    /// for a reference into a transient row.
    fn get_field(&self, doc_id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        Ok(self
            .get(doc_id)?
            .and_then(|document| document.get(field).cloned()))
    }

    /// Find the first document whose top-level field equals `value`.
    /// Persistent stores can override this with an indexed or JSON-path
    /// lookup so point updates do not have to materialise every row.
    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        for doc_id in self.doc_ids()? {
            if self.get_field(doc_id, field)?.as_ref() == Some(value) {
                return Ok(Some(doc_id));
            }
        }
        Ok(None)
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
        let Some(mut document) = self.get(doc_id)? else {
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
    fn get_many(&self, doc_ids: &[DocId]) -> StorageBackendResult<BTreeMap<DocId, Document>> {
        let mut out = BTreeMap::new();
        for doc_id in doc_ids {
            if let Some(document) = self.get(*doc_id)? {
                out.insert(*doc_id, document);
            }
        }
        Ok(out)
    }

    /// Fetch several top-level fields for many documents. The result
    /// vector is aligned with `fields`; missing fields come back as
    /// [`Value::Null`], ids without a document are absent. Persistent
    /// backends override this to extract all fields in one scan
    /// instead of materialising whole documents.
    fn get_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut out = BTreeMap::new();
        for doc_id in doc_ids {
            let Some(document) = self.get(*doc_id)? else {
                continue;
            };
            let values = fields
                .iter()
                .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                .collect();
            out.insert(*doc_id, values);
        }
        Ok(out)
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
    ) -> StorageBackendResult<()> {
        let mut projected = self.get_fields_multi(doc_ids, fields)?;
        for doc_id in doc_ids {
            let values = projected
                .remove(doc_id)
                .unwrap_or_else(|| vec![Value::Null; fields.len()]);
            if !visitor(*doc_id, values) {
                break;
            }
        }
        Ok(())
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
    ) -> StorageBackendResult<()> {
        self.for_each_fields_multi(doc_ids, fields, &mut |doc_id, values| {
            let references: Vec<&Value> = values.iter().collect();
            visitor(doc_id, &references)
        })
    }

    /// Bulk variant of [`DocumentStore::get_field`]. The default
    /// implementation walks each id one at a time; persistent backends
    /// should override to run a single batched query.
    fn get_fields_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, Value>> {
        let mut out = BTreeMap::new();
        for doc_id in doc_ids {
            out.insert(
                *doc_id,
                self.get_field(*doc_id, field)?.unwrap_or(Value::Null),
            );
        }
        Ok(out)
    }

    /// Return `true` if any document has `field == value`.
    fn has_value(&self, field: &str, value: &Value) -> StorageBackendResult<bool> {
        for doc_id in self.doc_ids()? {
            if self.get_field(doc_id, field)?.as_ref() == Some(value) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Find the first document whose top-level fields match every
    /// requested value.
    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        for doc_id in self.doc_ids()? {
            let mut matches = true;
            for (field, value) in fields.iter().zip(values) {
                if self.get_field(doc_id, field)?.unwrap_or(Value::Null) != *value {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Ok(Some(doc_id));
            }
        }
        Ok(None)
    }

    /// Evaluate a hierarchical path expression against a document.
    /// Matches UQA behavior for `DocumentStore.eval_path` /
    /// `HierarchicalDocument.eval_path` semantics.
    fn eval_path(
        &self,
        doc_id: DocId,
        path: &[PathSegment],
    ) -> StorageBackendResult<Option<Value>> {
        let Some(document) = self.get(doc_id)? else {
            return Ok(None);
        };
        Ok(eval_path_in_document(&document, path))
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>>;

    /// Return the first stored document id strictly greater than `after`, or
    /// the first id when `after` is `None`. Scan operators use this cursor API
    /// so a full table scan does not need a cardinality-sized id vector before
    /// it can yield its first row.
    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        Ok(self
            .doc_ids()?
            .into_iter()
            .filter(|doc_id| after.is_none_or(|after| *doc_id > after))
            .min())
    }

    fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        Ok(self.doc_ids()?.into_iter().max().unwrap_or(0))
    }

    fn len(&self) -> StorageBackendResult<usize>;

    fn is_empty(&self) -> StorageBackendResult<bool> {
        Ok(self.len()? == 0)
    }

    /// Iterate over `(doc_id, document)` pairs in id order. The default
    /// implementation fetches each document individually; SQLite-backed
    /// stores override with a single query.
    fn iter_all(&self) -> StorageBackendResult<Box<dyn Iterator<Item = (DocId, Document)> + '_>> {
        let mut ids = self.doc_ids()?;
        ids.sort_unstable();
        let snapshot = self.snapshot()?;
        let mut rows = Vec::with_capacity(ids.len());
        for doc_id in ids {
            if let Some(document) = snapshot.get(doc_id)? {
                rows.push((doc_id, document));
            }
        }
        Ok(Box::new(rows.into_iter()))
    }

    /// Read-only handle suitable for an `ExecutionContext`. Persistent
    /// backends share their connection; memory backends deep-clone so the
    /// snapshot is isolated from later mutations.
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>>;

    /// Independent writable copy used by the in-memory engine transaction
    /// rollback path. Persistent engines restore through their backend
    /// transaction and need not implement this operation.
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        Err(StorageBackendError::Other(
            "writable document-store snapshots are not supported by this backend".into(),
        ))
    }
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

fn projected_layout_slots(layout: &[String], fields: &[&str]) -> Vec<Option<usize>> {
    fields
        .iter()
        .map(|field| {
            layout
                .binary_search_by(|stored| stored.as_str().cmp(field))
                .ok()
        })
        .collect()
}

fn project_document_layout_ref<'a>(
    document: Option<&'a Document>,
    slots: &[Option<usize>],
    null: &'a Value,
    values: &mut Vec<&'a Value>,
) {
    values.clear();
    let Some(document) = document else {
        values.resize(slots.len(), null);
        return;
    };
    let mut stored_values = document.values().enumerate();
    let mut current = stored_values.next();
    for slot in slots {
        let Some(slot) = slot else {
            values.push(null);
            continue;
        };
        while current.is_some_and(|(stored_slot, _)| stored_slot < *slot) {
            current = stored_values.next();
        }
        values.push(match current {
            Some((stored_slot, value)) if stored_slot == *slot => value,
            _ => null,
        });
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
        let null = Value::Null;
        let mut values = Vec::with_capacity(fields.len());
        let fields_are_sorted = fields.windows(2).all(|pair| pair[0] <= pair[1]);
        let layout_slots: Vec<Vec<Option<usize>>> = if fields_are_sorted {
            self.layouts
                .iter()
                .map(|layout| projected_layout_slots(layout, fields))
                .collect()
        } else {
            Vec::new()
        };

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
            let mut layout_ids = self.document_layout_ids.range(doc_ids[0]..);
            let mut current_layout = layout_ids.next();
            for doc_id in doc_ids {
                while current.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                    current = documents.next();
                }
                while current_layout.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                    current_layout = layout_ids.next();
                }
                let document = match current {
                    Some((stored_id, document)) if stored_id == doc_id => Some(document),
                    _ => None,
                };
                let layout_id = match current_layout {
                    Some((stored_id, layout_id)) if stored_id == doc_id => Some(*layout_id),
                    _ => None,
                };
                if let Some(slots) = layout_id.and_then(|layout_id| layout_slots.get(layout_id)) {
                    project_document_layout_ref(document, slots, &null, &mut values);
                } else {
                    project_document_fields_ref(
                        document,
                        fields,
                        fields_are_sorted,
                        &null,
                        &mut values,
                    );
                }
                if !visitor(*doc_id, &values) {
                    return Ok(());
                }
            }
            return Ok(());
        }

        for doc_id in doc_ids {
            let document = self.documents.get(doc_id);
            let slots = self
                .document_layout_ids
                .get(doc_id)
                .and_then(|layout_id| layout_slots.get(*layout_id));
            if let Some(slots) = slots {
                project_document_layout_ref(document, slots, &null, &mut values);
            } else {
                project_document_fields_ref(
                    document,
                    fields,
                    fields_are_sorted,
                    &null,
                    &mut values,
                );
            }
            if !visitor(*doc_id, &values) {
                return Ok(());
            }
        }
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
        let got = s.get(1).unwrap().unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
    }

    #[test]
    fn get_field_returns_value() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))])).unwrap();
        assert_eq!(s.get_field(1, "year").unwrap(), Some(Value::Int(2026)));
        assert_eq!(s.get_field(1, "missing").unwrap(), None);
        assert_eq!(s.get_field(99, "year").unwrap(), None);
    }

    #[test]
    fn delete_removes_doc() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("a", Value::Int(1))])).unwrap();
        s.delete(1).unwrap();
        assert!(s.get(1).unwrap().is_none());
        assert_eq!(s.len().unwrap(), 0);
    }

    #[test]
    fn doc_ids_returns_all() {
        let mut s = MemoryDocumentStore::new();
        s.put(2, Document::new()).unwrap();
        s.put(1, Document::new()).unwrap();
        s.put(3, Document::new()).unwrap();
        assert_eq!(s.doc_ids().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn get_fields_bulk_returns_value_per_id_with_null_for_missing() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("year", Value::Int(2026))])).unwrap();
        s.put(2, doc([("year", Value::Int(2025))])).unwrap();
        let got = s.get_fields_bulk(&[1, 2, 99], "year").unwrap();
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

        let got = s
            .get_fields_multi(&[1, 99], &["title", "missing", "year"])
            .unwrap();
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
        })
        .unwrap();

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
        })
        .unwrap();

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
        )
        .unwrap();
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
        })
        .unwrap();
        assert_eq!(
            unsorted,
            vec![
                (10, vec![Value::Int(100), Value::Int(10)]),
                (1, vec![Value::Int(10), Value::Int(1)]),
            ]
        );
    }

    #[test]
    fn for_each_fields_multi_ref_uses_each_documents_layout() {
        let mut s = MemoryDocumentStore::new();
        s.put(
            1,
            doc([("alpha", Value::Int(1)), ("zulu", Value::Str("one".into()))]),
        )
        .unwrap();
        s.put(
            2,
            doc([
                ("alpha", Value::Int(2)),
                ("middle", Value::Str("two".into())),
            ]),
        )
        .unwrap();

        let mut projected = Vec::new();
        s.for_each_fields_multi_ref(&[1, 2], &["alpha", "zulu"], &mut |doc_id, values| {
            projected.push((
                doc_id,
                values.iter().map(|value| (*value).clone()).collect(),
            ));
            true
        })
        .unwrap();
        assert_eq!(
            projected,
            vec![
                (1, vec![Value::Int(1), Value::Str("one".into())]),
                (2, vec![Value::Int(2), Value::Null]),
            ]
        );

        s.patch_fields(
            2,
            &BTreeMap::from([
                ("middle".to_string(), Value::Null),
                ("zulu".to_string(), Value::Str("patched".into())),
            ]),
        )
        .unwrap();
        let mut patched = Vec::new();
        s.for_each_fields_multi_ref(&[2], &["alpha", "zulu"], &mut |_doc_id, values| {
            patched.extend(values.iter().map(|value| (*value).clone()));
            true
        })
        .unwrap();
        assert_eq!(patched, vec![Value::Int(2), Value::Str("patched".into())]);
    }

    #[test]
    fn has_value_returns_true_when_any_doc_matches() {
        let mut s = MemoryDocumentStore::new();
        s.put(1, doc([("color", Value::Str("red".into()))]))
            .unwrap();
        s.put(2, doc([("color", Value::Str("blue".into()))]))
            .unwrap();
        assert!(s.has_value("color", &Value::Str("red".into())).unwrap());
        assert!(!s.has_value("color", &Value::Str("green".into())).unwrap());
    }

    #[test]
    fn find_doc_id_by_field_returns_first_match() {
        let mut s = MemoryDocumentStore::new();
        s.put(3, doc([("public_id", Value::Str("m-3".into()))]))
            .unwrap();
        s.put(7, doc([("public_id", Value::Str("m-7".into()))]))
            .unwrap();

        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("m-7".into()))
                .unwrap(),
            Some(7)
        );
        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("missing".into()))
                .unwrap(),
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

        let got = s.get(1).unwrap().unwrap();
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
        assert_eq!(
            s.eval_path(1, &path).unwrap(),
            Some(Value::Str("alice".into()))
        );
    }

    #[test]
    fn iter_all_yields_in_id_order() {
        let mut s = MemoryDocumentStore::new();
        s.put(3, doc([("k", Value::Int(3))])).unwrap();
        s.put(1, doc([("k", Value::Int(1))])).unwrap();
        s.put(2, doc([("k", Value::Int(2))])).unwrap();
        let collected: Vec<u64> = s.iter_all().unwrap().map(|(id, _)| id).collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }
}
