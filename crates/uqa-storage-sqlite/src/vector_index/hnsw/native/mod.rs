//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native record adapters keep canonical vectors and HNSW generations on one logical boundary.

use uqa_storage::{
    hnsw_index::HNSWIndex, vector_index::VectorIndex, KeyValueBatch, StorageBackendResult,
};

use super::{
    mutation::{missing_metadata, next_revision},
    SQLiteHNSWIndex,
};
use crate::vector_index::native::NativeVectorRead;
use crate::Result;

mod cache;
mod loading;
mod writing;

#[cfg(test)]
mod tests;

pub(super) use cache::GraphIdentity;
pub(super) use loading::load_meta;
pub(super) use writing::drop_metadata;

impl SQLiteHNSWIndex {
    pub(in crate::vector_index::hnsw) fn native_snapshot(&self) -> Result<Option<Self>> {
        Ok(self.persistent.native_snapshot()?.map(|view| {
            let mut snapshot = self.clone();
            snapshot.persistent.retained = Some(view);
            snapshot
        }))
    }

    pub(in crate::vector_index::hnsw) fn initialize_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
    ) -> Result<()> {
        let expected = load_meta(read)?.map(|(_, _, _, revision)| revision);
        if expected.is_none() && self.require_persisted_graph {
            return Err(missing_metadata(self).into());
        }
        let read = read.owned(batch)?;
        let entries = read.vectors()?;
        let mut graph = HNSWIndex::with_params(self.persistent.dimensions, self.params)?;
        // Canonical rows are already ordered by document and vector ordinal.
        for vectors in entries.chunk_by(|a, b| a.0 == b.0) {
            read.snapshot.control.check()?;
            graph.add_many(
                vectors[0].0,
                vectors.iter().map(|(_, _, v)| v.clone()).collect(),
            )?;
        }
        writing::persist_delta(
            &read,
            batch,
            self,
            &graph.take_persistence_delta(),
            next_revision(expected)?,
        )
    }

    pub(in crate::vector_index::hnsw) fn mutate_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        mutate: impl FnOnce(&mut HNSWIndex) -> StorageBackendResult<()>,
        canonical: impl FnOnce(&NativeVectorRead<'_>, &mut dyn KeyValueBatch) -> Result<()>,
    ) -> Result<()> {
        let Some((_, _, _, revision)) = load_meta(read)? else {
            if self.require_persisted_graph {
                return Err(missing_metadata(self).into());
            }
            return canonical(read, batch);
        };
        let cached = self
            .cached_native_graph(read)?
            .ok_or_else(|| missing_metadata(self))?;
        canonical(read, batch)?;
        let mut candidate = cached.as_ref().clone();
        mutate(&mut candidate)?;
        let delta = candidate.take_persistence_delta();
        // Cache publication is read-side only: a candidate must never be tagged with a later session view.
        writing::persist_delta(read, batch, self, &delta, next_revision(Some(revision))?)
    }
}
