//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated query changes share immutable row sources instead of recopying private payloads. Rows that running commands staged stay in the commands' own tiers above the selected changes, so a read of them copies nothing.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::{DocId, Value};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{
    document_store::Document, DocumentMetadata, DocumentStore, RetainedDocumentFields,
    StorageBackendError, StorageBackendResult, StoredDocument,
};

mod desired;
mod identities;
mod layers;
mod projection;
pub use desired::DocumentSelection;
pub use identities::{
    ChangedIdentities, ChangedIdentitiesView, ChangedRowCounts, RelationGeneration,
};
pub use layers::Changes;
use layers::{ChangeRef, IdentityLayer, StagedLayers};
mod selection;
use selection::Selection;
mod vectors;
use vectors::CapturedRows;

#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Change {
    Deleted,
    Fields(RetainedDocumentFields, DocumentMetadata),
    Retained(Arc<dyn DocumentStore>),
    Captured(Arc<uqa_core::memory::Budgeted<CapturedRows>>, bool),
}

impl Change {
    fn present(&self) -> bool {
        !matches!(self, Self::Deleted | Self::Captured(_, false))
    }

    fn fields(&self) -> Option<&Document> {
        match self {
            Self::Fields(fields, _) => Some(fields),
            Self::Deleted | Self::Retained(_) | Self::Captured(_, _) => None,
        }
    }

    fn into_stored(self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(match self {
            Self::Deleted | Self::Captured(_, false) => None,
            Self::Fields(fields, metadata) => Some(StoredDocument::with_metadata(
                fields.into_document(),
                metadata,
            )),
            Self::Retained(source) => return source.get_stored(id),
            Self::Captured(source, true) => return source.documents.get_stored(id),
        })
    }

    fn retained_source(&self) -> Option<&Arc<dyn DocumentStore>> {
        match self {
            Self::Retained(source) => Some(source),
            Self::Captured(source, true) => Some(&source.documents),
            Self::Deleted | Self::Fields(_, _) | Self::Captured(_, false) => None,
        }
    }
}

/// A fixed selection of replacements and tombstones, and the rows running commands staged above it. Clones and document snapshots share both the selection and its allocation lease; fallible extensions charge replacement capacity while retaining the original source owners.
#[derive(Clone, Default)]
pub struct DocumentChanges {
    selection: Option<Arc<Selection>>,
    /// The rows a transaction changed, above the selection.
    identities: Option<Arc<IdentityLayer>>,
    /// The rows running commands staged, above everything else.
    staged: Option<Arc<StagedLayers>>,
}

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
        !self.rows().is_empty() || self.identities.is_some() || !self.staged_views().is_empty()
    }

    pub fn contains_change(&self, id: DocId) -> StorageBackendResult<bool> {
        Ok(self.layers_change(id)? || self.selected(id).is_some())
    }

    pub fn change_presence(&self, id: DocId) -> StorageBackendResult<Option<bool>> {
        Ok(self.get(id)?.map(|change| change.present()))
    }

    /// Visit every change in identity order, each with whether its row is present afterwards.
    pub fn changes(&self) -> Changes<bool, bool> {
        self.changes_after(None)
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
        if self.staged.is_some() || self.identities.is_some() {
            // Lazy layers stream a bounded page at a time; the selection below them is shared.
            return Box::new(self.change_rows_after(None).map(|change| {
                change.and_then(|(id, change)| change.into_stored(id).map(|row| (id, row)))
            }))
                as Box<dyn Iterator<Item = StorageBackendResult<(DocId, Option<StoredDocument>)>>>;
        }
        let (mut owned, shared) = match self.selection.map(Arc::try_unwrap) {
            Some(Ok(selection)) => {
                let (rows, capacity, allocation) = selection.into_parts();
                (
                    Some((rows.into_iter(), capacity, allocation)),
                    Self::default(),
                )
            }
            Some(Err(selection)) => (
                None,
                Self {
                    selection: Some(selection),
                    identities: None,
                    staged: None,
                },
            ),
            None => (None, Self::default()),
        };
        let mut position = 0;
        Box::new(std::iter::from_fn(move || {
            let (id, change) = if let Some((rows, _, _)) = owned.as_mut() {
                rows.next()?
            } else {
                let row = shared.rows().get(position)?.clone();
                position += 1;
                row
            };
            Some(change.into_stored(id).map(|row| (id, row)))
        }))
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
        self.get(id)?
            .map_or(Change::Deleted, ChangeRef::into_owned)
            .into_stored(id)
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        let mut rows = BTreeMap::new();
        let mut staged = self.layer_reader(ids);
        let mut index = 0;
        while index < ids.len() {
            if let Some((end, source)) = self.batch_source_run(ids, index, &mut staged)? {
                rows.extend(source.get_stored_many(&ids[index..end])?);
                index = end;
            } else {
                let id = ids[index];
                let change = self
                    .batch_change(&mut staged, id)?
                    .map_or(Change::Deleted, ChangeRef::into_owned);
                if let Some(row) = change.into_stored(id)? {
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
        let mut staged = self.layer_reader(ids);
        let mut index = 0;
        while index < ids.len() {
            control.check()?;
            if let Some((end, source)) = self.batch_source_run(ids, index, &mut staged)? {
                let (page, _memory) =
                    read_stored_documents(source.as_ref(), &ids[index..end], control)?.into_parts();
                for row in page {
                    control.check()?;
                    rows.push(row)?;
                }
                index = end;
            } else {
                let change = self.batch_change(&mut staged, ids[index])?;
                let row = match change.as_deref() {
                    Some(Change::Fields(fields, metadata)) => {
                        Some(RetainedStoredDocument::with_metadata(
                            fields.retain_with_control(control)?,
                            *metadata,
                        ))
                    }
                    Some(Change::Deleted | Change::Captured(_, false)) | None => None,
                    Some(Change::Retained(_) | Change::Captured(_, true)) => {
                        unreachable!("retained source run")
                    }
                };
                rows.push(row)?;
                index += 1;
            }
        }
        control.check()?;
        Ok(rows)
    }

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        Ok(match self.get(id)?.as_deref() {
            Some(Change::Fields(_, metadata)) => Some(*metadata),
            Some(Change::Retained(source)) => return source.get_metadata(id),
            Some(Change::Captured(source, true)) => return source.documents.get_metadata(id),
            Some(Change::Deleted | Change::Captured(_, false)) | None => None,
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
        let change = self.get(id)?;
        if let Some(source) = change.as_deref().and_then(Change::retained_source) {
            source.with_field_ref_controlled(id, field, control, visitor)?;
        } else {
            visitor(
                change
                    .as_deref()
                    .and_then(Change::fields)
                    .and_then(|fields| fields.get(field)),
            )?;
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
        let mut staged = self.layer_reader(ids);
        let mut index = 0;
        while index < ids.len() {
            control.check()?;
            if let Some((end, source)) = self.batch_source_run(ids, index, &mut staged)? {
                let page = uqa_storage::document_store::read_field_presence(
                    source.as_ref(),
                    &ids[index..end],
                    fields,
                    control,
                )?;
                present.extend_from_slice(&page)?;
                index = end;
            } else {
                let change = self.batch_change(&mut staged, ids[index])?;
                let row = change.as_deref().and_then(Change::fields);
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
        Ok(self.change_presence(id)? == Some(true))
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        let change = self.get(id)?;
        match change.as_deref().and_then(Change::retained_source) {
            Some(source) => source.get_field(id, field),
            None => Ok(change
                .as_deref()
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
        match self.batch_source_run(ids, 0, &mut self.layer_reader(ids))? {
            Some((end, source)) if end == ids.len() => source.get_shared_fields(ids, fields),
            _ => Ok(None),
        }
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut ids = Vec::new();
        for change in self.changes() {
            let (id, present) = change?;
            if present {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        let mut ids = Vec::new();
        for change in self.changes_after(after) {
            if ids.len() == limit {
                break;
            }
            let (id, present) = change?;
            if present {
                ids.push(id);
            }
        }
        Ok(ids)
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
        for change in self.changes_after(after) {
            control.check()?;
            if ids.len() == limit {
                break;
            }
            let (id, present) = change?;
            if present {
                ids.push(id)?;
            }
        }
        control.check()?;
        Ok(ids)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        let mut count = 0;
        for change in self.changes() {
            if change?.1 {
                count += 1;
            }
        }
        Ok(count)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
