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

    /// Visit a projection together with whether each requested document
    /// actually exists. This avoids a separate `contains_doc_id` probe when a
    /// caller must distinguish a missing document from an existing document
    /// whose requested fields are all NULL.
    fn for_each_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        if fields.is_empty() {
            for doc_id in doc_ids {
                if !visitor(*doc_id, self.contains_doc_id(*doc_id)?, &[]) {
                    break;
                }
            }
            return Ok(());
        }

        let projected = self.get_fields_multi(doc_ids, fields)?;
        let null = Value::Null;
        let missing = vec![&null; fields.len()];
        for doc_id in doc_ids {
            let Some(values) = projected.get(doc_id) else {
                if !visitor(*doc_id, false, &missing) {
                    break;
                }
                continue;
            };
            let references = values.iter().collect::<Vec<_>>();
            if !visitor(*doc_id, true, &references) {
                break;
            }
        }
        Ok(())
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

    /// Return up to `limit` document ids strictly greater than `after`, in
    /// ascending order. Scan operators use this bounded cursor instead of
    /// reacquiring their store lock and issuing one backend lookup per row.
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut doc_ids = self.doc_ids()?;
        doc_ids.sort_unstable();
        Ok(doc_ids
            .into_iter()
            .filter(|doc_id| after.is_none_or(|after| *doc_id > after))
            .take(limit)
            .collect())
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

mod memory;

pub use memory::MemoryDocumentStore;
