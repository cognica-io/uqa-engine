//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Layout-compiled borrowed and shared field projection.

use std::sync::Arc;

use super::{DocId, MemoryDocumentRow, MemoryDocumentStore, SharedDocumentRow, Value};

const MISSING_SLOT: usize = usize::MAX;

mod cache;
pub(super) use cache::ProjectionCache;
use cache::ProjectionPage;

impl MemoryDocumentStore {
    pub(super) fn visit_next_rows(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> usize {
        use std::ops::Bound::{Excluded, Unbounded};

        if limit == 0 {
            return 0;
        }
        let lower = after.map_or(Unbounded, Excluded);
        if fields.is_empty() {
            let mut visited = 0;
            for (doc_id, _) in self.state.documents.range((lower, Unbounded)).take(limit) {
                visited += 1;
                if !visitor(*doc_id, &[]) {
                    break;
                }
            }
            return visited;
        }
        let mut projection = self.projections.page(&self.state, fields);
        let null = Value::Null;
        let mut values = Vec::with_capacity(fields.len());
        let mut visited = 0usize;
        for (doc_id, stored) in self.state.documents.range((lower, Unbounded)).take(limit) {
            visited += 1;
            projection
                .layout(stored.layout_id)
                .project(stored, &null, &mut values);
            if !visitor(*doc_id, &values) {
                break;
            }
        }
        visited
    }

    pub(super) fn next_shared_rows(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> Vec<(DocId, SharedDocumentRow)> {
        use std::ops::Bound::{Excluded, Unbounded};

        if limit == 0 {
            return Vec::new();
        }
        let mut projection = self.projections.page(&self.state, fields);
        let lower = after.map_or(Unbounded, Excluded);
        self.state
            .documents
            .range((lower, Unbounded))
            .take(limit)
            .map(|(doc_id, stored)| {
                (
                    *doc_id,
                    SharedDocumentRow::new(
                        Arc::clone(&stored.values),
                        Arc::clone(&projection.layout(stored.layout_id).slots),
                    ),
                )
            })
            .collect()
    }

    pub(super) fn visit_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) {
        let null = Value::Null;
        let mut values = Vec::with_capacity(fields.len());
        let mut projection = self.projections.page(&self.state, fields);

        if should_merge_projected_scan(doc_ids, self.state.documents.len()) {
            self.visit_merge_scan(
                doc_ids,
                &mut projection,
                fields.len(),
                &null,
                &mut values,
                visitor,
            );
            return;
        }

        for doc_id in doc_ids {
            let stored = self.state.documents.get(doc_id);
            project_stored(
                stored,
                stored.map(|stored| projection.layout(stored.layout_id)),
                fields.len(),
                &null,
                &mut values,
            );
            if !visitor(*doc_id, stored.is_some(), &values) {
                return;
            }
        }
    }

    pub(super) fn shared_fields(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> Vec<Option<SharedDocumentRow>> {
        let mut projection = self.projections.page(&self.state, fields);
        doc_ids
            .iter()
            .map(|doc_id| {
                let stored = self.state.documents.get(doc_id)?;
                Some(SharedDocumentRow::new(
                    Arc::clone(&stored.values),
                    Arc::clone(&projection.layout(stored.layout_id).slots),
                ))
            })
            .collect()
    }

    fn visit_merge_scan<'a>(
        &'a self,
        doc_ids: &[DocId],
        projection: &mut ProjectionPage<'_>,
        requested_len: usize,
        null: &'a Value,
        values: &mut Vec<&'a Value>,
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) {
        let mut documents = self.state.documents.range(doc_ids[0]..);
        let mut current = documents.next();
        for doc_id in doc_ids {
            while current.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                current = documents.next();
            }
            let stored = match current {
                Some((stored_id, stored)) if stored_id == doc_id => Some(stored),
                _ => None,
            };
            project_stored(
                stored,
                stored.map(|stored| projection.layout(stored.layout_id)),
                requested_len,
                null,
                values,
            );
            if !visitor(*doc_id, stored.is_some(), values) {
                return;
            }
        }
    }
}

pub(super) fn should_merge_projected_scan(doc_ids: &[DocId], document_count: usize) -> bool {
    let Some((&first, &last)) = doc_ids.first().zip(doc_ids.last()) else {
        return false;
    };
    if doc_ids.len() < 2 || !doc_ids.windows(2).all(|pair| pair[0] <= pair[1]) {
        return false;
    }
    let probe_budget = (doc_ids.len() as u128).saturating_mul(8);
    let id_span = u128::from(last)
        .saturating_sub(u128::from(first))
        .saturating_add(1);
    id_span <= probe_budget || probe_budget >= document_count as u128
}

#[derive(Debug)]
struct ProjectedLayout {
    slots: Arc<[usize]>,
    complete: bool,
}

impl ProjectedLayout {
    fn compile(layout: &[String], fields: &[String]) -> Self {
        let slots: Arc<[usize]> = fields
            .iter()
            .map(|field| {
                layout
                    .binary_search_by(|stored| stored.as_str().cmp(field.as_str()))
                    .unwrap_or(MISSING_SLOT)
            })
            .collect();
        Self {
            complete: slots.iter().all(|slot| *slot != MISSING_SLOT),
            slots,
        }
    }

    fn project<'a>(
        &self,
        stored: &'a MemoryDocumentRow,
        null: &'a Value,
        values: &mut Vec<&'a Value>,
    ) {
        values.clear();
        if self.complete {
            values.extend(self.slots.iter().map(|slot| &stored.values[*slot]));
        } else {
            values.extend(self.slots.iter().map(|slot| {
                if *slot == MISSING_SLOT {
                    null
                } else {
                    &stored.values[*slot]
                }
            }));
        }
    }
}

fn project_stored<'a>(
    stored: Option<&'a MemoryDocumentRow>,
    projection: Option<&ProjectedLayout>,
    requested_len: usize,
    null: &'a Value,
    values: &mut Vec<&'a Value>,
) {
    if let (Some(stored), Some(projection)) = (stored, projection) {
        projection.project(stored, null, values);
    } else {
        values.clear();
        values.resize(requested_len, null);
    }
}
