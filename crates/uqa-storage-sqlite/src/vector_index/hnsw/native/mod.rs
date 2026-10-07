//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native record adapters keep canonical vectors and HNSW generations on one logical boundary.

use uqa_storage::{
    hnsw_index::{HNSWCanonicalBuilder, HNSWIndex, HNSWMutation},
    KeyValueBatch,
};

use super::{
    mutation::{missing_metadata, next_revision},
    SQLiteHNSWIndex,
};
use crate::vector_index::native::{publication::VectorPublication, NativeVectorRead};
use crate::Result;
use uqa_core::memory::Budgeted;

pub(super) type Candidate = (Budgeted<HNSWIndex>, u64);

mod cache;
mod loading;
mod records;
mod writing;

#[cfg(test)]
mod tests;

pub(super) use cache::GraphIdentity;
pub(super) use loading::load_meta;
pub(crate) use records::NativeHNSWRecords;
pub(super) use writing::drop_metadata;

impl SQLiteHNSWIndex {
    pub(in crate::vector_index::hnsw) fn write_native_graph(
        &self,
        operation: impl FnOnce(
            &NativeVectorRead<'_>,
            &mut dyn KeyValueBatch,
        ) -> Result<Option<Candidate>>,
    ) -> Result<bool> {
        if self.persistent.retained.is_some() {
            return Err(crate::SQLiteError::StorageBackend(
                "a retained vector snapshot is read-only".into(),
            ));
        }
        let result = self
            .persistent
            .conn
            .with_native_write_snapshot(|snapshot, batch| {
                operation(&NativeVectorRead::new(snapshot, &self.persistent)?, batch)
            })?;
        let Some((candidate, snapshot)) = result else {
            return Ok(false);
        };
        self.retain_native_candidate(
            candidate,
            snapshot.as_ref().map(|staged| &staged.snapshot),
            snapshot.as_ref().and_then(|staged| staged.committed),
        );
        Ok(true)
    }

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
    ) -> Result<Option<Candidate>> {
        let expected = load_meta(read)?.map(|(_, _, _, revision)| revision);
        if expected.is_none() && self.require_persisted_graph {
            return Err(missing_metadata(self).into());
        }
        let read = read.owned(batch)?;
        let mut builder = HNSWCanonicalBuilder::new(
            self.persistent.dimensions,
            self.params,
            &read.snapshot.control,
        )?;
        read.visit_ordered_vectors(|document, ordinal, vector| {
            Ok(builder.push(document, ordinal, vector)?)
        })?;
        let delta = builder.finish_delta()?;
        let revision = next_revision(expected)?;
        writing::persist_delta(
            &read,
            batch,
            self,
            &delta,
            revision,
            VectorPublication::Canonical,
        )?;
        let (delta, memory) = delta.into_parts();
        Ok(Some((Budgeted::new(delta.into_graph(), memory), revision)))
    }

    pub(in crate::vector_index::hnsw) fn mutate_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        mutation: HNSWMutation<'_>,
        canonical: impl FnOnce(&NativeVectorRead<'_>, &mut dyn KeyValueBatch) -> Result<()>,
    ) -> Result<Option<Candidate>> {
        let Some((_, _, _, revision)) = load_meta(read)? else {
            if self.require_persisted_graph {
                return Err(missing_metadata(self).into());
            }
            canonical(read, batch)?;
            return Ok(None);
        };
        let cached = self
            .cached_native_graph(read)?
            .ok_or_else(|| missing_metadata(self))?;
        // Admit the retained mutation before spending resources on a derived graph.
        // A failed candidate still discards this entire evaluated batch.
        let publication = if matches!(mutation, HNSWMutation::Clear) {
            VectorPublication::Canonical
        } else {
            let key = records::metadata_key(
                read.owner.expect("persisted native HNSW owner"),
                &read.index.field,
                &read.snapshot.control,
            )?;
            batch.hnsw_mutation(&key, mutation)?;
            VectorPublication::HNSWPreview
        };
        let delta = cached.prepare_delta(mutation, &read.snapshot.control)?;
        canonical(read, batch)?;
        let revision = next_revision(Some(revision))?;
        writing::persist_delta(read, batch, self, &delta, revision, publication)?;
        let (delta, memory) = delta.into_parts();
        Ok(Some((Budgeted::new(delta.into_graph(), memory), revision)))
    }
}
