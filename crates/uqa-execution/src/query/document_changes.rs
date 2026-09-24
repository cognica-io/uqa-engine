//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated query changes share immutable row sources instead of recopying private payloads.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::{DocId, Value};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{
    document_store::Document, DocumentMetadata, DocumentStore, RetainedDocumentFields,
    StorageBackendError, StorageBackendResult, StoredDocument,
};

mod desired;
mod projection;
pub use desired::DocumentSelection;
mod selection;
use selection::Selection;

#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Change {
    Deleted,
    Fields(RetainedDocumentFields, DocumentMetadata),
    Retained(Arc<dyn DocumentStore>),
}

impl Change {
    fn present(&self) -> bool {
        !matches!(self, Self::Deleted)
    }

    fn fields(&self) -> Option<&Document> {
        match self {
            Self::Fields(fields, _) => Some(fields),
            Self::Deleted | Self::Retained(_) => None,
        }
    }

    fn into_stored(self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(match self {
            Self::Deleted => None,
            Self::Fields(fields, metadata) => Some(StoredDocument::with_metadata(
                fields.into_document(),
                metadata,
            )),
            Self::Retained(source) => return source.get_stored(id),
        })
    }
}

/// A fixed selection of replacements and tombstones. Clones and document snapshots share both the selection and its allocation lease; fallible extensions charge replacement capacity while retaining the original source owners.
#[derive(Clone, Default)]
pub struct DocumentChanges(Option<Arc<Selection>>);

impl DocumentChanges {
    /// Serialized providers copy selected private rows while their live handle is guarded; immutable providers use `with_retained` instead.
    pub fn capture_owned(
        source: &dyn DocumentStore,
        desired: DocumentSelection,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut result = Self::default();
        let desired = desired.finish(control)?;
        let mut desired = desired.entries();
        loop {
            control.check()?;
            let mut page = uqa_core::memory::BudgetedVec::new(control.memory());
            for row in desired.by_ref().take(crate::DEFAULT_BATCH_SIZE) {
                page.push(row)?;
            }
            if page.is_empty() {
                break;
            }
            let mut ids = uqa_core::memory::BudgetedVec::new(control.memory());
            for (id, present) in page.iter() {
                if *present {
                    ids.push(*id)?;
                }
            }
            let (documents, _page_memory) =
                uqa_storage::document_store::read_stored_documents(source, &ids, control)?
                    .into_parts();
            let mut documents = documents.into_iter();
            for (id, present) in page.iter().copied() {
                control.check()?;
                let row = if present {
                    documents.next().expect("validated whole-row page length")
                } else {
                    None
                };
                let change = row.map_or(Change::Deleted, |row| {
                    let (fields, metadata) = row.into_parts();
                    Change::Fields(fields, metadata)
                });
                result.insert(id, change, control)?;
            }
        }
        Ok(result)
    }

    pub fn has_changes(&self) -> bool {
        !self.rows().is_empty()
    }

    pub fn contains_change(&self, id: DocId) -> bool {
        self.get(id).is_some()
    }

    pub fn change_presence(&self, id: DocId) -> Option<bool> {
        self.get(id).map(Change::present)
    }

    pub fn changes(&self) -> impl Iterator<Item = (DocId, bool)> + '_ {
        self.rows()
            .iter()
            .map(|(id, change)| (*id, change.present()))
    }

    pub fn changes_after(&self, after: Option<DocId>) -> impl Iterator<Item = (DocId, bool)> + '_ {
        self.rows()[self
            .rows()
            .partition_point(|(id, _)| after.is_some_and(|after| *id <= after))..]
            .iter()
            .map(|(id, change)| (*id, change.present()))
    }

    /// `source` must already retain an immutable snapshot. Membership checks normalize missing replacements into tombstones without reading complete row payloads.
    pub fn with_retained(
        mut self,
        source: Arc<dyn DocumentStore>,
        desired: DocumentSelection,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let desired = desired.finish(control)?;
        let mut newer = Self::default();
        for (id, present) in desired.entries() {
            control.check()?;
            let change = if present && source.contains_doc_id(id)? {
                Change::Retained(Arc::clone(&source))
            } else {
                Change::Deleted
            };
            newer.insert(id, change, control)?;
        }
        self.extend(newer, control)?;
        Ok(self)
    }

    pub fn insert_shared(
        &mut self,
        id: DocId,
        document: Option<(Arc<Document>, DocumentMetadata)>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let control = self.retention_control(control);
        let document = document
            .map(|(fields, metadata)| {
                RetainedDocumentFields::new(fields, &control).map(|fields| (fields, metadata))
            })
            .transpose()?;
        self.insert(
            id,
            document.map_or(Change::Deleted, |(fields, metadata)| {
                Change::Fields(fields, metadata)
            }),
            &control,
        )
    }

    fn insert_owned(
        &mut self,
        id: DocId,
        row: Option<StoredDocument>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.insert_shared(
            id,
            row.map(|row| {
                let (fields, metadata) = row.into_parts();
                (Arc::new(fields), metadata)
            }),
            control,
        )
    }

    /// Capture already charged immutable fields without copying or reserving their payload again.
    pub fn from_retained(
        rows: impl IntoIterator<Item = (DocId, Option<(RetainedDocumentFields, DocumentMetadata)>)>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut selected = Self::default();
        for (id, row) in rows {
            selected.insert(
                id,
                row.map_or(Change::Deleted, |(fields, metadata)| {
                    Change::Fields(fields, metadata)
                }),
                control,
            )?;
        }
        control.check()?;
        Ok(selected)
    }

    /// Capture evaluated fields without copying their payloads, then merge the complete selection with an older view.
    pub fn from_shared(
        rows: impl IntoIterator<Item = (DocId, Option<(Arc<Document>, DocumentMetadata)>)>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut selected = Self::default();
        for (id, row) in rows {
            selected.insert_shared(id, row, control)?;
        }
        control.check()?;
        Ok(selected)
    }

    pub fn from_rows(
        rows: BTreeMap<DocId, Option<StoredDocument>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut selected = Self::default();
        for (id, row) in rows {
            selected.insert_owned(id, row, control)?;
        }
        control.check()?;
        Ok(selected)
    }

    pub fn into_rows(
        self,
    ) -> impl Iterator<Item = StorageBackendResult<(DocId, Option<StoredDocument>)>> {
        let (mut owned, shared) = match self.0.map(Arc::try_unwrap) {
            Some(Ok(selection)) => {
                let (rows, capacity, allocation) = selection.into_parts();
                (
                    Some((rows.into_iter(), capacity, allocation)),
                    Self::default(),
                )
            }
            Some(Err(selection)) => (None, Self(Some(selection))),
            None => (None, Self::default()),
        };
        let mut position = 0;
        std::iter::from_fn(move || {
            let (id, change) = if let Some((rows, _, _)) = owned.as_mut() {
                rows.next()?
            } else {
                let row = shared.rows().get(position)?.clone();
                position += 1;
                row
            };
            Some(change.into_stored(id).map(|row| (id, row)))
        })
    }

    fn source_run<'a>(
        &'a self,
        ids: &[DocId],
        start: usize,
    ) -> Option<(usize, &'a Arc<dyn DocumentStore>)> {
        let Change::Retained(source) = self.get(ids[start])? else {
            return None;
        };
        let mut end = start + 1;
        while end < ids.len()
            && matches!(self.get(ids[end]), Some(Change::Retained(next)) if Arc::ptr_eq(source, next))
        {
            end += 1;
        }
        Some((end, source))
    }
}

fn readonly() -> StorageBackendError {
    StorageBackendError::Other("captured document changes are read-only".into())
}

impl DocumentStore for DocumentChanges {
    fn put(&mut self, _: DocId, _: Document) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn patch_fields(
        &mut self,
        _: DocId,
        _: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        Err(readonly())
    }

    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(readonly())
    }

    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.get(id)
            .cloned()
            .unwrap_or(Change::Deleted)
            .into_stored(id)
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        let mut rows = BTreeMap::new();
        let mut index = 0;
        while index < ids.len() {
            if let Some((end, source)) = self.source_run(ids, index) {
                rows.extend(source.get_stored_many(&ids[index..end])?);
                index = end;
            } else {
                let id = ids[index];
                if let Some(row) = self.get_stored(id)? {
                    rows.insert(id, row);
                }
                index += 1;
            }
        }
        Ok(rows)
    }

    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::RetainedDocumentPage> {
        use uqa_storage::{document_store::read_stored_documents, RetainedStoredDocument};
        control.check()?;
        let mut rows = uqa_core::memory::BudgetedVec::new(control.memory());
        rows.reserve(ids.len())?;
        let mut index = 0;
        while index < ids.len() {
            control.check()?;
            if let Some((end, source)) = self.source_run(ids, index) {
                let (page, _memory) =
                    read_stored_documents(source.as_ref(), &ids[index..end], control)?.into_parts();
                for row in page {
                    control.check()?;
                    rows.push(row)?;
                }
                index = end;
            } else {
                let row = match self.get(ids[index]) {
                    Some(Change::Fields(fields, metadata)) => {
                        Some(RetainedStoredDocument::with_metadata(
                            fields.retain_with_control(control)?,
                            *metadata,
                        ))
                    }
                    Some(Change::Deleted) | None => None,
                    Some(Change::Retained(_)) => unreachable!("retained source run"),
                };
                rows.push(row)?;
                index += 1;
            }
        }
        control.check()?;
        Ok(rows)
    }

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        Ok(match self.get(id) {
            Some(Change::Fields(_, metadata)) => Some(*metadata),
            Some(Change::Retained(source)) => return source.get_metadata(id),
            Some(Change::Deleted) | None => None,
        })
    }

    fn with_field_ref_controlled(
        &self,
        id: DocId,
        field: &str,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(Option<&Value>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        match self.get(id) {
            Some(Change::Retained(source)) => {
                source.with_field_ref_controlled(id, field, control, visitor)?;
            }
            change => visitor(
                change
                    .and_then(Change::fields)
                    .and_then(|fields| fields.get(field)),
            )?,
        }
        control.check()
    }

    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<bool>> {
        control.check()?;
        let mut present = uqa_core::memory::BudgetedVec::new(control.memory());
        if fields.is_empty() {
            return Ok(present);
        }
        let mut index = 0;
        while index < ids.len() {
            control.check()?;
            if let Some((end, source)) = self.source_run(ids, index) {
                let page = uqa_storage::document_store::read_field_presence(
                    source.as_ref(),
                    &ids[index..end],
                    fields,
                    control,
                )?;
                present.extend_from_slice(&page)?;
                index = end;
            } else {
                let row = self.get(ids[index]).and_then(Change::fields);
                for field in fields {
                    control.check()?;
                    present.push(row.is_some_and(|row| row.contains_key(*field)))?;
                }
                index += 1;
            }
        }
        control.check()?;
        Ok(present)
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        Ok(self.change_presence(id) == Some(true))
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        match self.get(id) {
            Some(Change::Retained(source)) => source.get_field(id, field),
            change => Ok(change
                .and_then(Change::fields)
                .and_then(|fields| fields.get(field))
                .cloned()),
        }
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
        if ids.is_empty() {
            return Ok(Some(Vec::new()));
        }
        match self.source_run(ids, 0) {
            Some((end, source)) if end == ids.len() => source.get_shared_fields(ids, fields),
            _ => Ok(None),
        }
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(self
            .changes()
            .filter_map(|(id, present)| present.then_some(id))
            .collect())
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        Ok(self
            .changes_after(after)
            .filter_map(|(id, present)| present.then_some(id))
            .take(limit)
            .collect())
    }

    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        control.check()?;
        let mut ids = uqa_core::memory::BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(ids);
        }
        for (id, present) in self.changes_after(after) {
            control.check()?;
            if ids.len() == limit {
                break;
            }
            if present {
                ids.push(id)?;
            }
        }
        control.check()?;
        Ok(ids)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.changes().filter(|(_, present)| *present).count())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
