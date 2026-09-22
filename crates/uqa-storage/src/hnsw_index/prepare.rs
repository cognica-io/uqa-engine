//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated HNSW candidates retain their allocation allowance without changing the source graph.

mod workspace;

use super::{HNSWIndex, HNSWNodeSnapshot, HNSWPersistenceDelta};
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
        let memory = workspace::candidate(self, 0, control)?;
        Ok(Budgeted::new(self.clone_controlled(control)?, memory))
    }

    /// Prepare an immutable graph delta. Failure or cancellation leaves the source and its dirty-node state intact.
    pub fn prepare_delta(
        &self,
        mutation: HNSWMutation<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWPersistenceDelta>> {
        self.prepare_delta_changes(std::slice::from_ref(&mutation), control)
    }

    pub(crate) fn prepare_delta_changes(
        &self,
        mutations: &[HNSWMutation<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWPersistenceDelta>> {
        control.check()?;
        if matches!(mutations, [HNSWMutation::Clear]) {
            return Self::with_params(self.dimensions, self.params)?.delta_controlled(control);
        }
        let additions = workspace::validate_mutations(self.dimensions, mutations, control)?;
        let _workspace = workspace::candidate(self, additions, control)?;
        let mut candidate = self.clone_controlled(control)?;
        for mutation in mutations {
            control.check()?;
            match *mutation {
                HNSWMutation::Replace { document, vectors } => {
                    let mut owned = Vec::with_capacity(vectors.len());
                    for vector in vectors {
                        control.check()?;
                        owned.push(vector.clone());
                    }
                    candidate.replace_document_vectors(document, owned, Some(control))?;
                }
                HNSWMutation::Delete(document) => {
                    candidate.mark_document_deleted(document, Some(control))?;
                    candidate.maybe_rebuild(Some(control))?;
                }
                HNSWMutation::Clear => candidate.clear()?,
            }
        }
        candidate.delta_controlled(control)
    }

    /// Rebuild from complete canonical tensors ordered by document and ordinal. Reconstruction reserves its graph and algorithm workspace before copying any vector.
    pub fn prepare_canonical(
        dimensions: u32,
        params: HNSWIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWPersistenceDelta>> {
        Self::from_canonical_controlled(dimensions, params, vectors, control)?
            .delta_controlled(control)
    }

    /// Build an immutable graph from ordered canonical tensors, retaining the construction allowance with the graph.
    pub(crate) fn from_canonical_controlled(
        dimensions: u32,
        params: HNSWIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let source = Self::with_params(dimensions, params)?;
        let memory = workspace::candidate(&source, vectors.len(), control)?;
        let mut candidate = source;
        let mut previous: Option<(DocId, u32)> = None;
        for (document, ordinal, vector) in vectors {
            control.check()?;
            let valid = match previous {
                Some((old_document, old_ordinal)) if old_document == *document => {
                    old_ordinal.checked_add(1) == Some(*ordinal)
                }
                Some((old_document, _)) => old_document < *document && *ordinal == 0,
                None => *ordinal == 0,
            };
            if !valid {
                return Err(crate::StorageBackendError::Other(
                    "HNSW canonical tensors require ordered complete ordinals".into(),
                ));
            }
            crate::vector_index::validate_vector_values(dimensions, vector)?;
            candidate.insert_vector(*document, *ordinal, vector.clone(), Some(control))?;
            previous = Some((*document, *ordinal));
        }
        Ok(Budgeted::new(candidate, memory))
    }

    fn clone_controlled(&self, control: &StorageReadControl) -> StorageBackendResult<Self> {
        let mut candidate = Self::with_params(self.dimensions, self.params)?;
        candidate.entry_point = self.entry_point;
        candidate.max_level = self.max_level;
        candidate.next_node_id = self.next_node_id;
        candidate.deleted_count = self.deleted_count;
        candidate.full_rewrite = self.full_rewrite;
        for (id, node) in &self.nodes {
            control.check()?;
            candidate.nodes.insert(*id, node.clone());
        }
        for (key, node) in &self.active {
            control.check()?;
            candidate.active.insert(*key, *node);
        }
        for id in &self.dirty_nodes {
            control.check()?;
            candidate.dirty_nodes.insert(*id);
        }
        Ok(candidate)
    }

    fn delta_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<HNSWPersistenceDelta>> {
        control.check()?;
        let include = |id: &u64| self.full_rewrite || self.dirty_nodes.contains(id);
        let mut bytes = 0;
        let mut count = 0;
        for node in self.nodes.values().filter(|node| include(&node.id)) {
            control.check()?;
            workspace::add(&mut bytes, workspace::snapshot(node)?)?;
            count += 1;
        }
        let memory = control.memory().reserve(bytes)?;
        let mut nodes = Vec::with_capacity(count);
        for node in self.nodes.values().filter(|node| include(&node.id)) {
            control.check()?;
            nodes.push(HNSWNodeSnapshot::from(node));
        }
        Ok(Budgeted::new(
            HNSWPersistenceDelta {
                meta: self.graph_meta(),
                nodes,
                full_rewrite: self.full_rewrite,
            },
            memory,
        ))
    }
}

pub(super) fn check(control: Control<'_>) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}
