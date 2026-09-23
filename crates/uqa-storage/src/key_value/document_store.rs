//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document access and mutations use one provider-owned view of the exact storage name.

mod migration;
mod projection;
mod read;

use std::collections::BTreeMap;
use std::sync::Arc;

use super::codec::{document_key, document_key_prefix, encode_stored_document_value, other_error};
use super::{table_owners, KeyValueBatch, KeyValueRead, KeyValueStore};
use crate::document_store::Document;
use crate::read_control::StorageReadControl;
use crate::{DocumentStore, StorageBackendResult, StoredDocument};
use read::Documents;
use uqa_core::memory::BudgetedVec;
use uqa_core::{DocId, Value};

#[derive(Clone)]
enum Source {
    Live(Arc<dyn KeyValueStore>),
    Retained(Arc<dyn KeyValueRead + Send + Sync>),
}

/// Document store implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueDocumentStore {
    source: Source,
    table: String,
}

impl KeyValueDocumentStore {
    pub fn new(store: Arc<dyn KeyValueStore>, table: impl Into<String>) -> Self {
        Self {
            source: Source::Live(store),
            table: table.into(),
        }
    }

    fn read<T>(
        &self,
        query: impl FnOnce(Documents<'_>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let mut query = Some(query);
        let mut result = None;
        let mut visit = |read: &dyn KeyValueRead| {
            let query = query
                .take()
                .ok_or_else(|| other_error("document read was evaluated twice"))?;
            result = Some(query(Documents {
                read,
                table: &self.table,
            })?);
            Ok(())
        };
        match &self.source {
            Source::Live(store) => store.with_read_view(&mut visit)?,
            Source::Retained(read) => visit(&**read)?,
        }
        result.ok_or_else(|| other_error("document read was not evaluated"))
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(
            Documents<'_>,
            &mut dyn KeyValueBatch,
            Option<table_owners::Owner>,
        ) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let Source::Live(store) = &self.source else {
            return Err(other_error("document snapshot is read-only"));
        };
        let durable = store.identifier_allocator().is_some();
        let mut change = Some(change);
        let mut result = None;
        store.with_mutation(&mut |read, batch| {
            let change = change
                .take()
                .ok_or_else(|| other_error("document mutation was evaluated twice"))?;
            let owner = if durable {
                Some(table_owners::document_owner(read, batch, &self.table)?)
            } else {
                None
            };
            result = Some(change(
                Documents {
                    read,
                    table: &self.table,
                },
                batch,
                owner,
            )?);
            Ok(())
        })?;
        result.ok_or_else(|| other_error("document mutation was not evaluated"))
    }
}

fn put_document(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    owner: Option<table_owners::Owner>,
    id: DocId,
    document: StoredDocument,
) -> StorageBackendResult<()> {
    let (mut fields, metadata) = document.into_parts();
    fields.retain(|_, value| !matches!(value, Value::Null));
    let bytes = encode_stored_document_value(&StoredDocument::with_metadata(fields, metadata))?;
    if let Some(owner) = owner {
        owner.observe(batch, id)?;
    }
    batch.put(&document_key(table, id)?, &bytes)
}

impl DocumentStore for KeyValueDocumentStore {
    fn put(&mut self, id: DocId, document: Document) -> StorageBackendResult<()> {
        self.mutate(|view, batch, owner| {
            let metadata = view
                .get(id)?
                .map(|document| document.metadata())
                .unwrap_or_default();
            put_document(
                batch,
                view.table,
                owner,
                id,
                StoredDocument::with_metadata(document, metadata),
            )
        })
    }

    fn put_stored(&mut self, id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.mutate(|view, batch, owner| put_document(batch, view.table, owner, id, document))
    }

    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.read(|view| view.get(id))
    }

    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<crate::RetainedDocumentPage> {
        control.check()?;
        if ids.is_empty() {
            return Ok(BudgetedVec::new(control.memory()));
        }
        self.read(|view| view.retained_many_controlled(ids, control))
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        self.read(|view| view.many(ids))
    }

    fn get_many(&self, ids: &[DocId]) -> StorageBackendResult<BTreeMap<DocId, Document>> {
        Ok(self
            .get_stored_many(ids)?
            .into_iter()
            .map(|(id, document)| (id, document.into_fields()))
            .collect())
    }

    fn get_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        self.read(|view| view.project(ids, fields))
    }

    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit_projection(ids, fields, visitor)
    }

    fn for_each_fields_multi_ref(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit_projection(ids, fields, &mut |id, _, values| visitor(id, values))
    }

    fn for_each_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) -> StorageBackendResult<()> {
        self.visit_projection(ids, fields, &mut |id, _, values| {
            visitor(id, values.iter().map(|value| (*value).clone()).collect())
        })
    }

    fn get_fields_bulk(
        &self,
        ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, Value>> {
        let projected = self.get_fields_multi(ids, &[field])?;
        Ok(ids
            .iter()
            .map(|id| {
                (
                    *id,
                    projected
                        .get(id)
                        .and_then(|row| row.first().cloned())
                        .unwrap_or(Value::Null),
                )
            })
            .collect())
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.read(|view| view.contains(id))
    }

    fn patch_fields(
        &mut self,
        id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        self.mutate(|view, batch, owner| {
            let Some(mut document) = view.get(id)? else {
                return Ok(false);
            };
            for (field, value) in updates {
                if matches!(value, Value::Null) {
                    document.fields_mut().remove(field);
                } else {
                    document.fields_mut().insert(field.clone(), value.clone());
                }
            }
            put_document(batch, view.table, owner, id, document)?;
            Ok(true)
        })
    }

    fn delete(&mut self, id: DocId) -> StorageBackendResult<()> {
        self.mutate(|view, batch, _| batch.delete(&document_key(view.table, id)?))
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.mutate(|view, batch, owner| {
            if let Some(owner) = owner {
                owner.fence(batch)?;
            }
            batch.delete_prefix(&document_key_prefix(view.table)?)
        })
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.next_doc_ids(None, usize::MAX)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        Ok(self.next_doc_ids(after, 1)?.into_iter().next())
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        self.read(|view| view.ids(after, limit))
    }

    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        control.check()?;
        if limit == 0 {
            return Ok(BudgetedVec::new(control.memory()));
        }
        self.read(|view| view.id_page_controlled(after, limit, control))
    }

    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        if !fields.is_empty() {
            return Ok(None);
        }
        let (ids, control) =
            self.read(|view| Ok((view.id_page(after, limit)?, view.read.control().clone())))?;
        let mut visited = 0;
        for id in ids.iter().copied() {
            control.check()?;
            visited += 1;
            let keep_going = visitor(id, &[]);
            control.check()?;
            if !keep_going {
                break;
            }
        }
        control.check()?;
        Ok(Some(visited))
    }

    fn len(&self) -> StorageBackendResult<usize> {
        self.read(|view| view.count())
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        self.read(|view| view.find(|document| document.get(field) == Some(value)))
    }

    fn has_value(&self, field: &str, value: &Value) -> StorageBackendResult<bool> {
        Ok(self.find_doc_id_by_field(field, value)?.is_some())
    }

    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        self.read(|view| {
            view.find(|document| {
                fields
                    .iter()
                    .zip(values)
                    .all(|(field, value)| document.get(field).unwrap_or(&Value::Null) == value)
            })
        })
    }

    fn iter_all(&self) -> StorageBackendResult<Box<dyn Iterator<Item = (DocId, Document)> + '_>> {
        let rows = self.read(|view| view.all())?;
        Ok(Box::new(rows.into_iter()))
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        let source = match &self.source {
            Source::Live(_) => self.read(|view| {
                Ok(Source::Retained(
                    view.read.retain(&[&document_key_prefix(view.table)?])?,
                ))
            })?,
            Source::Retained(_) => self.source.clone(),
        };
        Ok(Arc::new(Self {
            source,
            table: self.table.clone(),
        }))
    }
}
