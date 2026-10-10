//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate roots share immutable graph pages and publish only successful evaluations.

pub(super) mod workspace;

use super::{store::Record, HNSWGraphDelta, HNSWIndex};
use crate::{read_control::StorageReadControl, HNSWIndexParams, StorageBackendResult, VectorIndex};
use uqa_core::{memory::Budgeted, DocId};

pub(super) type Control<'a> = Option<&'a StorageReadControl>;

/// Canonical values already evaluated by the caller. Preparing them never invokes application code.
#[derive(Clone, Copy)]
pub enum HNSWMutation<'a> {
    Replace {
        document: DocId,
        vectors: &'a [Vec<f32>],
    },
    Delete(DocId),
    Clear,
}

impl HNSWIndex {
    pub(super) fn snapshot_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Self>())?;
        if self.memory.shares_allowance(control.memory()) {
            return Ok(Budgeted::new(self.clone(), memory));
        }
        let mut candidate = Self::with_memory(self.dimensions, self.params, control.memory())?;
        candidate.entry_point = self.entry_point;
        candidate.max_level = self.max_level;
        candidate.next_node_id = self.next_node_id;
        candidate.deleted_count = self.deleted_count;
        candidate.full_rewrite = self.full_rewrite;
        for entry in self.nodes.iter() {
            control.check()?;
            let (id, node) = entry?;
            let _copy = control.memory().reserve(node.memory_bytes()?)?;
            candidate.nodes.insert(id, (*node).clone(), Some(control))?;
        }
        for (source, target) in [
            (&self.raw_vectors, &mut candidate.raw_vectors),
            (&self.normalized_vectors, &mut candidate.normalized_vectors),
        ] {
            for entry in source.iter() {
                control.check()?;
                let (id, vector) = entry?;
                let _copy = control.memory().reserve(vector.memory_bytes()?)?;
                target.insert(id, (*vector).clone(), Some(control))?;
            }
        }
        for entry in self.active.iter() {
            control.check()?;
            let (key, node) = entry?;
            candidate.active.insert(key, *node, Some(control))?;
        }
        for entry in self.dirty_nodes.iter() {
            control.check()?;
            let (id, value) = entry?;
            candidate.dirty_nodes.insert(id, *value, Some(control))?;
        }
        Ok(Budgeted::new(candidate, memory))
    }

    /// Prepare a streaming immutable delta. Failure leaves source graph and pending persistence unchanged.
    pub fn prepare_delta(
        &self,
        mutation: HNSWMutation<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWGraphDelta>> {
        self.prepare_delta_changes(std::slice::from_ref(&mutation), control)
    }

    pub(crate) fn prepare_delta_changes(
        &self,
        mutations: &[HNSWMutation<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWGraphDelta>> {
        control.check()?;
        if matches!(mutations, [HNSWMutation::Clear]) {
            return Ok(Budgeted::new(
                Self::with_memory(self.dimensions, self.params, control.memory())?.delta(control),
                control.memory().empty_reservation(),
            ));
        }
        workspace::validate_mutations(self.dimensions, mutations, control)?;
        self.prepare_delta_stream(control, |visit| {
            for mutation in mutations {
                visit(*mutation)?;
            }
            Ok(())
        })
    }

    /// Replay evaluated mutations directly into a private candidate. A failed read, validation or allocation publishes no graph.
    pub(crate) fn prepare_delta_stream(
        &self,
        control: &StorageReadControl,
        replay: impl FnOnce(
            &mut dyn FnMut(HNSWMutation<'_>) -> StorageBackendResult<()>,
        ) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<Budgeted<HNSWGraphDelta>> {
        let _workspace = workspace::operation(self, control)?;
        let (mut candidate, memory) = self.snapshot_controlled(control)?.into_parts();
        replay(&mut |mutation| {
            workspace::validate_mutations(
                self.dimensions,
                std::slice::from_ref(&mutation),
                control,
            )?;
            match mutation {
                HNSWMutation::Replace { document, vectors } => {
                    candidate.replace_document_vectors(document, vectors, Some(control))?;
                }
                HNSWMutation::Delete(document) => {
                    candidate.mark_document_deleted(document, Some(control))?;
                    candidate.maybe_rebuild(Some(control))?;
                }
                HNSWMutation::Clear => candidate.clear()?,
            }
            Ok(())
        })?;
        control.check()?;
        Ok(Budgeted::new(candidate.delta(control), memory))
    }

    /// Rebuild from ordered canonical tensors without an additional corpus or graph copy. Streaming providers use `HNSWCanonicalBuilder` directly.
    pub fn prepare_canonical(
        dimensions: u32,
        params: HNSWIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWGraphDelta>> {
        let (index, memory) =
            Self::from_canonical_controlled(dimensions, params, vectors, control)?.into_parts();
        Ok(Budgeted::new(index.delta(control), memory))
    }

    pub(crate) fn from_canonical_controlled(
        dimensions: u32,
        params: HNSWIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        let mut builder = HNSWCanonicalBuilder::new(dimensions, params, control)?;
        for (document, ordinal, vector) in vectors {
            builder.push(*document, *ordinal, vector)?;
        }
        builder.finish()
    }
}

/// One ordered canonical vector at a time under the original storage allowance.
pub struct HNSWCanonicalBuilder {
    index: HNSWIndex,
    control: StorageReadControl,
    memory: uqa_core::memory::MemoryReservation,
    previous: Option<(DocId, u32)>,
    failed: bool,
}

impl HNSWCanonicalBuilder {
    pub fn new(
        dimensions: u32,
        params: HNSWIndexParams,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<HNSWIndex>())?;
        Ok(Self {
            index: HNSWIndex::with_memory(dimensions, params, control.memory())?,
            control: control.clone(),
            memory,
            previous: None,
            failed: false,
        })
    }

    pub fn push(
        &mut self,
        document: DocId,
        ordinal: u32,
        vector: &[f32],
    ) -> StorageBackendResult<()> {
        self.check_usable()?;
        let result = self.push_inner(document, ordinal, vector);
        self.failed = result.is_err();
        result
    }

    fn push_inner(
        &mut self,
        document: DocId,
        ordinal: u32,
        vector: &[f32],
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let valid = match self.previous {
            Some((old_document, old_ordinal)) if old_document == document => {
                old_ordinal.checked_add(1) == Some(ordinal)
            }
            Some((old_document, _)) => old_document < document && ordinal == 0,
            None => ordinal == 0,
        };
        if !valid {
            return Err(crate::StorageBackendError::Other(
                "HNSW canonical tensors require ordered complete ordinals".into(),
            ));
        }
        crate::vector_index::validate_vector_values(self.index.dimensions, vector)?;
        let _workspace = workspace::operation(&self.index, &self.control)?;
        self.index
            .insert_vector(document, ordinal, vector.to_vec(), Some(&self.control))?;
        self.previous = Some((document, ordinal));
        Ok(())
    }

    fn check_usable(&self) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.failed {
            return Err(crate::StorageBackendError::Other(
                "HNSW construction cannot continue after an error".into(),
            ));
        }
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<Budgeted<HNSWIndex>> {
        self.check_usable()?;
        Ok(Budgeted::new(self.index, self.memory))
    }

    pub fn finish_delta(self) -> StorageBackendResult<Budgeted<HNSWGraphDelta>> {
        self.check_usable()?;
        Ok(Budgeted::new(self.index.delta(&self.control), self.memory))
    }
}

pub(super) fn check(control: Control<'_>) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}
