//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable query rows share their retained source and adapt only requested documents.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId, Value,
};
use uqa_storage::{
    read_control::StorageReadControl, DocumentMetadata, DocumentStore, StorageBackendError,
    StorageBackendResult, StoredDocument,
};

use super::layout::RowLayout;
use crate::query::document_changes::DocumentChanges;

mod identifiers;
mod projection;

struct State {
    source: Arc<dyn DocumentStore>,
    layout: RowLayout,
    private_layout: RowLayout,
    changes: DocumentChanges,
    count: usize,
    control: StorageReadControl,
}

#[derive(Clone)]
pub(super) struct RetainedDocuments(Arc<Budgeted<State>>);

impl RetainedDocuments {
    fn checked_read<T>(
        &self,
        read: impl FnOnce() -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        self.0.control.check()?;
        let result = read()?;
        self.0.control.check()?;
        Ok(result)
    }

    pub(super) fn new(
        source: Arc<dyn DocumentStore>,
        layout: RowLayout,
        private_layout: RowLayout,
        changes: DocumentChanges,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut count = source.len()?;
        for (id, replacement) in changes.changes() {
            control.check()?;
            let present = source.contains_doc_id(id)?;
            count = match (present, replacement) {
                (true, false) => count.checked_sub(1),
                (false, true) => count.checked_add(1),
                _ => Some(count),
            }
            .ok_or_else(|| StorageBackendError::Other("query document count overflow".into()))?;
        }
        control.check()?;
        let state = State {
            source,
            layout,
            private_layout,
            changes,
            count,
            control: control.clone(),
        };
        Ok(Self(
            Budgeted::new(state, control.memory().empty_reservation()).into_shared()?,
        ))
    }

    fn visit_ids(
        &self,
        mut visitor: impl FnMut(DocId) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<()> {
        let mut after = None;
        loop {
            let ids = self.id_page(after, crate::DEFAULT_BATCH_SIZE)?;
            let Some(last) = ids.last().copied() else {
                return Ok(());
            };
            for id in ids.iter().copied() {
                self.0.control.check()?;
                if !visitor(id)? {
                    return Ok(());
                }
            }
            after = Some(last);
        }
    }

    fn find_id(
        &self,
        mut matches: impl FnMut(DocId) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Option<DocId>> {
        let mut found = None;
        self.visit_ids(|id| {
            if matches(id)? {
                found = Some(id);
            }
            Ok(found.is_none())
        })?;
        Ok(found)
    }
}

fn readonly() -> StorageBackendError {
    StorageBackendError::Other("query document snapshots are read-only".into())
}

fn layout_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("query row layout", error)
}

impl DocumentStore for RetainedDocuments {
    fn put(
        &mut self,
        _id: DocId,
        _document: uqa_storage::document_store::Document,
    ) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn put_stored(&mut self, _id: DocId, _document: StoredDocument) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn patch_fields(
        &mut self,
        _id: DocId,
        _updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        Err(readonly())
    }

    fn delete(&mut self, _id: DocId) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.checked_read(|| {
            if self.0.changes.contains_change(id) {
                self.0
                    .changes
                    .get_stored(id)?
                    .map(|row| self.0.layout.complete_private(row).map_err(layout_error))
                    .transpose()
            } else {
                self.0
                    .source
                    .get_stored(id)?
                    .map(|row| self.0.layout.adapt_base(row).map_err(layout_error))
                    .transpose()
            }
        })
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        let base_ids = self.selected_ids(ids, false)?;
        let base = self.0.source.get_stored_many(&base_ids)?;
        drop(base_ids);
        let mut rows = BTreeMap::new();
        for (id, row) in base {
            rows.insert(id, self.0.layout.adapt_base(row).map_err(layout_error)?);
        }
        let private_ids = self.selected_ids(ids, true)?;
        for (id, row) in self.0.changes.get_stored_many(&private_ids)? {
            rows.insert(
                id,
                self.0.layout.complete_private(row).map_err(layout_error)?,
            );
        }
        self.0.control.check()?;
        Ok(rows)
    }

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.checked_read(|| {
            if self.0.changes.contains_change(id) {
                self.0.changes.get_metadata(id)
            } else {
                self.0.source.get_metadata(id)
            }
        })
    }

    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<bool>> {
        self.0.control.check()?;
        control.check()?;
        let mut present = BudgetedVec::new(control.memory());
        if fields.is_empty() {
            return Ok(present);
        }
        for id in ids {
            self.0.control.check()?;
            control.check()?;
            let private = self.0.changes.contains_change(*id);
            let (source, layout): (&dyn DocumentStore, &RowLayout) = if private {
                (&self.0.changes, &self.0.private_layout)
            } else {
                (self.0.source.as_ref(), &self.0.layout)
            };
            let row_ids = uqa_storage::document_store::read_document_ids(
                source,
                id.checked_sub(1),
                1,
                control,
            )?;
            let exists = row_ids.first() == Some(id);
            let mut unknown = BudgetedVec::new(control.memory());
            for field in fields {
                control.check()?;
                if !layout.field_is_declared(field) && !layout.field_is_removed(field) {
                    unknown.push(*field)?;
                }
            }
            let physical = if exists {
                uqa_storage::document_store::read_field_presence(source, &[*id], &unknown, control)?
            } else {
                BudgetedVec::new(control.memory())
            };
            let mut index = 0;
            for field in fields {
                self.0.control.check()?;
                control.check()?;
                let value = if !exists || layout.field_is_removed(field) {
                    false
                } else if layout.field_is_declared(field) {
                    true
                } else {
                    let value = physical[index];
                    index += 1;
                    value
                };
                present.push(value)?;
            }
        }
        self.0.control.check()?;
        control.check()?;
        Ok(present)
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.checked_read(|| match self.0.changes.change_presence(id) {
            Some(present) => Ok(present),
            None => self.0.source.contains_doc_id(id),
        })
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        self.checked_read(|| {
            if self.0.changes.contains_change(id) {
                self.0.private_layout.base_field(&self.0.changes, id, field)
            } else {
                self.0.layout.base_field(self.0.source.as_ref(), id, field)
            }
        })
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        self.find_id(|id| Ok(self.get_field(id, field)?.as_ref() == Some(value)))
    }

    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        self.0.control.check()?;
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        self.find_id(|id| {
            for (field, value) in fields.iter().zip(values) {
                if self.get_field(id, field)?.unwrap_or(Value::Null) != *value {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    fn has_value(&self, field: &str, value: &Value) -> StorageBackendResult<bool> {
        Ok(self.find_doc_id_by_field(field, value)?.is_some())
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

    fn get_shared_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<uqa_storage::SharedDocumentRow>>>> {
        let Some(projection) = self.0.layout.projection(fields)? else {
            return Ok(None);
        };
        let Some(sources) = projection.shared_sources()? else {
            return Ok(None);
        };
        for id in ids {
            self.0.control.check()?;
            if self.0.changes.contains_change(*id) {
                return Ok(None);
            }
        }
        let rows = self.checked_read(|| self.0.source.get_shared_fields(ids, &sources))?;
        if rows.as_ref().is_some_and(|rows| {
            rows.iter()
                .flatten()
                .any(|row| row.with_projected(|values| projection.needs_shared_fallback(values)))
        }) {
            return Ok(None);
        }
        self.0.control.check()?;
        Ok(rows)
    }

    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::SharedDocumentRow)>>> {
        let Some(projection) = self.0.layout.projection(fields)? else {
            return Ok(None);
        };
        if self.0.changes.has_changes() {
            return Ok(None);
        }
        let Some(sources) = projection.shared_sources()? else {
            return Ok(None);
        };
        let rows =
            self.checked_read(|| self.0.source.next_shared_fields(after, limit, &sources))?;
        if rows.as_ref().is_some_and(|rows| {
            rows.iter().any(|(_, row)| {
                row.with_projected(|values| projection.needs_shared_fallback(values))
            })
        }) {
            return Ok(None);
        }
        self.0.control.check()?;
        Ok(rows)
    }

    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        let ids = self.id_page(after, limit)?;
        let mut visited = 0;
        self.for_each_fields_multi_ref(&ids, fields, &mut |id, values| {
            visited += 1;
            visitor(id, values)
        })?;
        Ok(Some(visited))
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut ids = BudgetedVec::new(self.0.control.memory());
        loop {
            let page = self.id_page(ids.last().copied(), crate::DEFAULT_BATCH_SIZE)?;
            if page.is_empty() {
                break;
            }
            ids.extend_from_slice(&page)?;
        }
        let (ids, _memory) = ids.into_parts();
        Ok(ids)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        Ok(self.id_page(after, 1)?.first().copied())
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        let (ids, _memory) = self.id_page(after, limit)?.into_parts();
        Ok(ids)
    }

    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.id_page_controlled(after, limit, control)
    }

    fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        let mut last = 0;
        self.visit_ids(|id| {
            last = id;
            Ok(true)
        })?;
        Ok(last)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        self.0.control.check()?;
        Ok(self.0.count)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.0.control.check()?;
        Ok(Arc::new(self.clone()))
    }

    fn retained_snapshot(&self) -> StorageBackendResult<Option<Arc<dyn DocumentStore>>> {
        self.snapshot().map(Some)
    }
}
