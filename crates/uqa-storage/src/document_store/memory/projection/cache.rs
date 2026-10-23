//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One read-owned projection compiles only encountered immutable layouts.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

use super::{super::MemoryDocumentState, ProjectedLayout};

#[derive(Debug, Default)]
pub(in crate::document_store::memory) struct ProjectionCache {
    current: Mutex<Option<Arc<Projection>>>,
}

#[derive(Debug)]
struct Projection {
    fields: Box<[String]>,
    layouts: Mutex<HashMap<usize, Arc<ProjectedLayout>>>,
}

pub(super) struct ProjectionPage<'a> {
    state: &'a MemoryDocumentState,
    projection: Arc<Projection>,
    last: Option<(usize, Arc<ProjectedLayout>)>,
}

impl ProjectionCache {
    pub(super) fn page<'a>(
        &self,
        state: &'a MemoryDocumentState,
        fields: &[&str],
    ) -> ProjectionPage<'a> {
        let mut current = self.current.lock();
        let matching = current.as_ref().filter(|projection| {
            projection.fields.len() == fields.len()
                && projection
                    .fields
                    .iter()
                    .zip(fields)
                    .all(|(left, right)| left == right)
        });
        let projection = if let Some(projection) = matching {
            Arc::clone(projection)
        } else {
            let projection = Arc::new(Projection {
                fields: fields.iter().map(|field| (*field).to_owned()).collect(),
                layouts: Mutex::default(),
            });
            *current = Some(Arc::clone(&projection));
            projection
        };
        // The cache retains only the most recent projection. An active page
        // owns its prior mapping through reentry; no lock spans a callback.
        ProjectionPage {
            state,
            projection,
            last: None,
        }
    }
}

impl ProjectionPage<'_> {
    pub(super) fn layout(&mut self, id: usize) -> &ProjectedLayout {
        // Zero-column rows share the same empty mapping regardless of shape.
        let id = if self.projection.fields.is_empty() {
            0
        } else {
            id
        };
        if self
            .last
            .as_ref()
            .is_none_or(|(previous, _)| *previous != id)
        {
            let mut layouts = self.projection.layouts.lock();
            let layout = layouts.entry(id).or_insert_with(|| {
                #[cfg(test)]
                COMPILED.with(|count| count.set(count.get() + 1));
                Arc::new(ProjectedLayout::compile(
                    &self.state.layouts[id],
                    &self.projection.fields,
                ))
            });
            self.last = Some((id, Arc::clone(layout)));
        }
        &self.last.as_ref().expect("selected layout").1
    }
}

#[cfg(test)]
thread_local! {
    static COMPILED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
