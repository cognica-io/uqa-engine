//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable query rows share their retained source and adapt only requested documents.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;
use uqa_core::{DocId, Value};
use uqa_storage::{
    DocumentMetadata, DocumentStore, StorageBackendError, StorageBackendResult, StoredDocument,
};

use super::layout::RowLayout;

mod projection;

struct State {
    source: Arc<dyn DocumentStore>,
    layout: RowLayout,
    changes: BTreeMap<DocId, Option<StoredDocument>>,
    count: usize,
}

#[derive(Clone)]
pub(super) struct RetainedDocuments(Arc<State>);

impl RetainedDocuments {
    pub(super) fn new(
        source: Arc<dyn DocumentStore>,
        layout: RowLayout,
        changes: BTreeMap<DocId, Option<StoredDocument>>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Self> {
        cancellation.check()?;
        let mut count = source.len()?;
        for (id, row) in &changes {
            cancellation.check()?;
            let present = source.contains_doc_id(*id)?;
            count = match (present, row.is_some()) {
                (true, false) => count.checked_sub(1),
                (false, true) => count.checked_add(1),
                _ => Some(count),
            }
            .ok_or_else(|| StorageBackendError::Other("query document count overflow".into()))?;
        }
        Ok(Self(Arc::new(State {
            source,
            layout,
            changes,
            count,
        })))
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
        match self.0.changes.get(&id) {
            Some(row) => row
                .as_ref()
                .map(|row| {
                    self.0
                        .layout
                        .complete_private(row.clone())
                        .map_err(layout_error)
                })
                .transpose(),
            None => self
                .0
                .source
                .get_stored(id)?
                .map(|row| self.0.layout.adapt_base(row).map_err(layout_error))
                .transpose(),
        }
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        let base_ids = ids
            .iter()
            .filter(|id| !self.0.changes.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        let base = self.0.source.get_stored_many(&base_ids)?;
        let mut rows = BTreeMap::new();
        for (id, row) in base {
            rows.insert(id, self.0.layout.adapt_base(row).map_err(layout_error)?);
        }
        for id in ids {
            if let Some(Some(row)) = self.0.changes.get(id) {
                rows.insert(
                    *id,
                    self.0
                        .layout
                        .complete_private(row.clone())
                        .map_err(layout_error)?,
                );
            }
        }
        Ok(rows)
    }

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        match self.0.changes.get(&id) {
            Some(row) => Ok(row.as_ref().map(StoredDocument::metadata)),
            None => self.0.source.get_metadata(id),
        }
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        match self.0.changes.get(&id) {
            Some(row) => Ok(row.is_some()),
            None => self.0.source.contains_doc_id(id),
        }
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        match self.0.changes.get(&id) {
            Some(Some(row)) => self.0.layout.private_field(row, field),
            Some(None) => Ok(None),
            None => self.0.layout.base_field(self.0.source.as_ref(), id, field),
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
        let Some(projection) = self.0.layout.projection(fields) else {
            return Ok(None);
        };
        if !projection.only_sources() || ids.iter().any(|id| self.0.changes.contains_key(id)) {
            return Ok(None);
        }
        let rows = self.0.source.get_shared_fields(ids, &projection.sources)?;
        if rows.as_ref().is_some_and(|rows| {
            rows.iter()
                .flatten()
                .any(|row| row.with_projected(|values| projection.needs_presence(values)))
        }) {
            return Ok(None);
        }
        Ok(rows)
    }

    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::SharedDocumentRow)>>> {
        let Some(projection) = self.0.layout.projection(fields) else {
            return Ok(None);
        };
        if !self.0.changes.is_empty() || !projection.only_sources() {
            return Ok(None);
        }
        let rows = self
            .0
            .source
            .next_shared_fields(after, limit, &projection.sources)?;
        if rows.as_ref().is_some_and(|rows| {
            rows.iter()
                .any(|(_, row)| row.with_projected(|values| projection.needs_presence(values)))
        }) {
            return Ok(None);
        }
        Ok(rows)
    }

    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        let ids = self.next_doc_ids(after, limit)?;
        let mut visited = 0;
        self.for_each_fields_multi_ref(&ids, fields, &mut |id, values| {
            visited += 1;
            visitor(id, values)
        })?;
        Ok(Some(visited))
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut ids = Vec::new();
        loop {
            let page = self.next_doc_ids(ids.last().copied(), crate::DEFAULT_BATCH_SIZE)?;
            if page.is_empty() {
                break;
            }
            ids.extend(page);
        }
        Ok(ids)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        Ok(self.next_doc_ids(after, 1)?.into_iter().next())
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut base = Vec::new();
        let mut cursor = after;
        while base.len() < limit {
            let page = self
                .0
                .source
                .next_doc_ids(cursor, (limit - base.len()).min(crate::DEFAULT_BATCH_SIZE))?;
            let Some(last) = page.last().copied() else {
                break;
            };
            if cursor.is_some_and(|cursor| page[0] <= cursor)
                || page.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(StorageBackendError::Other(
                    "query document page did not advance in id order".into(),
                ));
            }
            cursor = Some(last);
            base.extend(
                page.into_iter()
                    .filter(|id| !self.0.changes.contains_key(id)),
            );
        }
        let private = self
            .0
            .changes
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .filter_map(|(id, row)| row.as_ref().map(|_| *id))
            .take(limit);
        let mut base = base.into_iter().peekable();
        let mut private = private.peekable();
        let mut ids = Vec::new();
        while ids.len() < limit {
            let id = match (base.peek(), private.peek()) {
                (Some(left), Some(right)) if left < right => base.next(),
                (_, Some(_)) => private.next(),
                (Some(_), None) => base.next(),
                (None, None) => break,
            };
            ids.extend(id);
        }
        Ok(ids)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.0.count)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
