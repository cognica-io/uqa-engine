//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated query changes share immutable row sources instead of recopying private payloads.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;
use uqa_core::{CancellationToken, DocId, Value};
use uqa_storage::{
    document_store::Document, DocumentMetadata, DocumentStore, StorageBackendError,
    StorageBackendResult, StoredDocument,
};

mod projection;

#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Change {
    Deleted,
    Owned(Arc<StoredDocument>),
    Shared(Arc<Document>, DocumentMetadata),
    Retained(Arc<dyn DocumentStore>),
}

impl Change {
    fn present(&self) -> bool {
        !matches!(self, Self::Deleted)
    }

    fn fields(&self) -> Option<&Document> {
        match self {
            Self::Owned(row) => Some(row.fields()),
            Self::Shared(fields, _) => Some(fields),
            Self::Deleted | Self::Retained(_) => None,
        }
    }

    fn into_stored(self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(match self {
            Self::Deleted => None,
            Self::Owned(row) => Some(Arc::unwrap_or_clone(row)),
            Self::Shared(fields, metadata) => Some(StoredDocument::with_metadata(
                Arc::unwrap_or_clone(fields),
                metadata,
            )),
            Self::Retained(source) => return source.get_stored(id),
        })
    }
}

/// A fixed selection of replacements and tombstones. Clones and document snapshots share both the selection and its immutable sources; extensions use copy-on-write metadata without cloning payloads.
#[derive(Clone, Default)]
pub struct DocumentChanges(Arc<BTreeMap<DocId, Change>>);

impl DocumentChanges {
    /// Serialized providers copy selected private rows while their live handle is guarded; immutable providers use `with_retained` instead.
    pub fn capture_owned(
        source: &dyn DocumentStore,
        desired: BTreeMap<DocId, bool>,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<Self> {
        let mut result = BTreeMap::new();
        let mut desired = desired.into_iter();
        loop {
            cancellation.check()?;
            let page = desired
                .by_ref()
                .take(crate::DEFAULT_BATCH_SIZE)
                .collect::<Vec<_>>();
            if page.is_empty() {
                break;
            }
            let ids = page
                .iter()
                .filter_map(|(id, present)| present.then_some(*id))
                .collect::<Vec<_>>();
            let mut documents = source.get_stored_many(&ids)?;
            for (id, present) in page {
                result.insert(id, present.then(|| documents.remove(&id)).flatten());
            }
        }
        Ok(Self::from(result))
    }

    pub fn has_changes(&self) -> bool {
        !self.0.is_empty()
    }

    pub fn contains_change(&self, id: DocId) -> bool {
        self.0.contains_key(&id)
    }

    pub fn change_presence(&self, id: DocId) -> Option<bool> {
        self.0.get(&id).map(Change::present)
    }

    pub fn changes(&self) -> impl Iterator<Item = (DocId, bool)> + '_ {
        self.0.iter().map(|(id, change)| (*id, change.present()))
    }

    pub fn changes_after(&self, after: Option<DocId>) -> impl Iterator<Item = (DocId, bool)> + '_ {
        self.0
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .map(|(id, change)| (*id, change.present()))
    }

    /// `source` must already retain an immutable snapshot. Membership checks normalize missing replacements into tombstones without reading complete row payloads.
    pub fn with_retained(
        mut self,
        source: Arc<dyn DocumentStore>,
        desired: BTreeMap<DocId, bool>,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<Self> {
        cancellation.check()?;
        let changes = Arc::make_mut(&mut self.0);
        for (id, present) in desired {
            cancellation.check()?;
            let change = if present && source.contains_doc_id(id)? {
                Change::Retained(Arc::clone(&source))
            } else {
                Change::Deleted
            };
            changes.insert(id, change);
        }
        cancellation.check()?;
        Ok(self)
    }

    pub fn insert_shared(
        &mut self,
        id: DocId,
        document: Option<(Arc<Document>, DocumentMetadata)>,
    ) {
        Arc::make_mut(&mut self.0).insert(
            id,
            document.map_or(Change::Deleted, |(fields, metadata)| {
                Change::Shared(fields, metadata)
            }),
        );
    }

    pub fn extend(&mut self, newer: Self) {
        Arc::make_mut(&mut self.0).extend(Arc::unwrap_or_clone(newer.0));
    }

    pub fn into_rows(
        self,
    ) -> impl Iterator<Item = StorageBackendResult<(DocId, Option<StoredDocument>)>> {
        Arc::unwrap_or_clone(self.0)
            .into_iter()
            .map(|(id, change)| change.into_stored(id).map(|row| (id, row)))
    }

    fn source_run<'a>(
        &'a self,
        ids: &[DocId],
        start: usize,
    ) -> Option<(usize, &'a Arc<dyn DocumentStore>)> {
        let Change::Retained(source) = self.0.get(&ids[start])? else {
            return None;
        };
        let mut end = start + 1;
        while end < ids.len()
            && matches!(self.0.get(&ids[end]), Some(Change::Retained(next)) if Arc::ptr_eq(source, next))
        {
            end += 1;
        }
        Some((end, source))
    }
}

impl From<BTreeMap<DocId, Option<StoredDocument>>> for DocumentChanges {
    fn from(rows: BTreeMap<DocId, Option<StoredDocument>>) -> Self {
        Self(Arc::new(
            rows.into_iter()
                .map(|(id, row)| {
                    (
                        id,
                        row.map_or(Change::Deleted, |row| Change::Owned(Arc::new(row))),
                    )
                })
                .collect(),
        ))
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
        self.0
            .get(&id)
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

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        Ok(match self.0.get(&id) {
            Some(Change::Owned(row)) => Some(row.metadata()),
            Some(Change::Shared(_, metadata)) => Some(*metadata),
            Some(Change::Retained(source)) => return source.get_metadata(id),
            Some(Change::Deleted) | None => None,
        })
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        Ok(self.change_presence(id) == Some(true))
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        match self.0.get(&id) {
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

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.changes().filter(|(_, present)| *present).count())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
