//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copied document snapshots retain their payload and entry capacity through the last reader.

use std::{collections::BTreeMap, sync::Arc};

use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryReservation},
    ordering::sort_by_with_control,
    DocId, Value,
};

use super::{Document, DocumentMetadata, DocumentStore, StoredDocument};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

type Documents = Vec<(DocId, StoredDocument)>;

/// Adopt caller-owned rows without copying payloads. Finalization orders unique document identities and transfers the same allowance into immutable readers. Input decoding and mutable output allocations belong to their respective producers and callers.
pub struct RetainedDocumentStoreBuilder {
    entries: BudgetedVec<(DocId, StoredDocument)>,
    payload: MemoryReservation,
    control: StorageReadControl,
}

impl RetainedDocumentStoreBuilder {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            entries: BudgetedVec::new(control.memory()),
            payload: control.memory().empty_reservation(),
            control: control.clone(),
        }
    }

    pub fn add_document(
        &mut self,
        id: DocId,
        document: StoredDocument,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let (fields, metadata) = document.into_parts();
        let value = Value::Map(fields);
        let memory = value
            .reserve_retained_payload(self.control.memory(), self.control.cancellation())
            .map_err(super::retained::retention_error)?;
        // Drop pending fields before returning their payload reservation on failure.
        let pending = (value, memory);
        self.entries.reserve(1)?;
        self.control.check()?;
        let (Value::Map(fields), memory) = pending else {
            unreachable!("document field map");
        };
        self.entries
            .push((id, StoredDocument::with_metadata(fields, metadata)))?;
        self.payload.absorb(memory);
        Ok(())
    }

    pub fn finish(mut self) -> StorageBackendResult<RetainedDocumentStore> {
        sort_by_with_control(
            &mut self.entries,
            &mut || self.control.check(),
            |left, right, _| Ok(left.0.cmp(&right.0)),
        )?;
        for pair in self.entries.windows(2) {
            self.control.check()?;
            if pair[0].0 == pair[1].0 {
                return Err(StorageBackendError::Other(
                    "retained document input repeats a document identity".into(),
                ));
            }
        }
        self.control.check()?;
        let (entries, mut memory) = self.entries.into_parts();
        memory.absorb(self.payload);
        Ok(RetainedDocumentStore {
            entries: Budgeted::new(entries, memory).into_shared()?,
            control: self.control,
        })
    }
}

/// An immutable corpus whose nested snapshots share one entry/payload lease. Borrowed projections reserve only their reference buffer; field maps, tuple metadata and value buffers stay in the original corpus.
#[derive(Clone)]
pub struct RetainedDocumentStore {
    entries: Arc<Budgeted<Documents>>,
    control: StorageReadControl,
}

impl RetainedDocumentStore {
    fn document(&self, id: DocId) -> Option<&StoredDocument> {
        self.entries
            .binary_search_by_key(&id, |entry| entry.0)
            .ok()
            .map(|index| &self.entries[index].1)
    }

    fn after(&self, after: Option<DocId>) -> &[(DocId, StoredDocument)] {
        &self.entries[self
            .entries
            .partition_point(|(id, _)| after.is_some_and(|after| *id <= after))..]
    }

    fn visit<'a>(
        &'a self,
        rows: impl Iterator<Item = (DocId, Option<&'a StoredDocument>)>,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let mut projected = BudgetedVec::new(self.control.memory());
        projected.reserve(fields.len())?;
        for (id, document) in rows {
            self.control.check()?;
            projected.clear();
            for field in fields {
                self.control.check()?;
                projected.push(
                    document
                        .and_then(|document| document.fields().get(*field))
                        .unwrap_or(&Value::Null),
                )?;
            }
            if !visitor(id, document.is_some(), &projected) {
                break;
            }
        }
        self.control.check()
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained document snapshot".into())
}

impl DocumentStore for RetainedDocumentStore {
    fn put(&mut self, _: DocId, _: Document) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn patch_fields(&mut self, _: DocId, _: &Document) -> StorageBackendResult<bool> {
        Err(read_only())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }

    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.control.check()?;
        Ok(self.document(id).cloned())
    }
    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.control.check()?;
        Ok(self.document(id).map(StoredDocument::metadata))
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.control.check()?;
        Ok(self.document(id).is_some())
    }
    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        self.control.check()?;
        Ok(self
            .document(id)
            .and_then(|document| document.fields().get(field).cloned()))
    }

    fn get_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut rows = BTreeMap::new();
        self.for_each_fields_multi_ref_with_presence(ids, fields, &mut |id, present, values| {
            if present {
                rows.insert(id, values.iter().map(|value| (*value).clone()).collect());
            }
            true
        })?;
        Ok(rows)
    }
    fn for_each_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) -> StorageBackendResult<()> {
        self.for_each_fields_multi_ref(ids, fields, &mut |id, values| {
            visitor(id, values.iter().map(|value| (*value).clone()).collect())
        })
    }
    fn for_each_fields_multi_ref(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.for_each_fields_multi_ref_with_presence(ids, fields, &mut |id, _, values| {
            visitor(id, values)
        })
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit(
            ids.iter().map(|id| (*id, self.document(*id))),
            fields,
            visitor,
        )
    }
    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        let mut visited = 0;
        self.visit(
            self.after(after)
                .iter()
                .take(limit)
                .map(|(id, row)| (*id, Some(row))),
            fields,
            &mut |id, _, values| {
                visited += 1;
                visitor(id, values)
            },
        )?;
        Ok(Some(visited))
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        self.control.check()?;
        for (id, document) in self.entries.iter() {
            self.control.check()?;
            if document.fields().get(field) == Some(value) {
                return Ok(Some(*id));
            }
        }
        Ok(None)
    }
    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        self.control.check()?;
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        for (id, document) in self.entries.iter() {
            self.control.check()?;
            let mut matched = true;
            for (field, value) in fields.iter().zip(values) {
                self.control.check()?;
                if document.fields().get(field).unwrap_or(&Value::Null) != value {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Ok(Some(*id));
            }
        }
        Ok(None)
    }
    fn has_value(&self, field: &str, value: &Value) -> StorageBackendResult<bool> {
        Ok(self.find_doc_id_by_field(field, value)?.is_some())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.next_doc_ids(None, self.entries.len())
    }
    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        self.control.check()?;
        Ok(self.after(after).first().map(|(id, _)| *id))
    }
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        self.control.check()?;
        let mut ids = BudgetedVec::new(self.control.memory());
        for (id, _) in self.after(after).iter().take(limit) {
            self.control.check()?;
            ids.push(*id)?;
        }
        let (ids, _memory) = ids.into_parts();
        Ok(ids)
    }
    fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        self.control.check()?;
        Ok(self.entries.last().map_or(0, |(id, _)| *id))
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.control.check()?;
        Ok(self.entries.len())
    }
    fn iter_all(&self) -> StorageBackendResult<Box<dyn Iterator<Item = (DocId, Document)> + '_>> {
        self.control.check()?;
        Ok(Box::new(
            self.entries
                .iter()
                .map(|(id, document)| (*id, document.fields().clone())),
        ))
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.control.check()?;
        Ok(Arc::new(self.clone()))
    }
}

#[cfg(test)]
mod tests;
