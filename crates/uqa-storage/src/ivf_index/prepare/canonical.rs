//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded canonical construction and detached training preserve immutable cached generations.

use super::{workspace_bytes, Budgeted, DocId, IVFIndex, IVFMetadataSnapshot, IVFState};
use crate::{
    read_control::StorageReadControl, vector_index::IVFIndexParams, StorageBackendError,
    StorageBackendResult,
};

impl IVFIndex {
    pub(crate) fn from_canonical_controlled(
        dimensions: u32,
        params: IVFIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        params.validate()?;
        let memory = control.memory().reserve(workspace_bytes(
            dimensions,
            vectors.len(),
            params.nlist.min(vectors.len()),
        )?)?;
        let mut index = Self::with_params(
            dimensions,
            params.nlist,
            params.nprobe,
            params.train_threshold,
        );
        let mut previous = None;
        for tensors in vectors.chunk_by(|left, right| left.0 == right.0) {
            control.check()?;
            let document = tensors[0].0;
            if previous.is_some_and(|previous| previous >= document) {
                return Err(invalid_ordinals());
            }
            let mut values = Vec::with_capacity(tensors.len());
            for (expected, (_, ordinal, vector)) in tensors.iter().enumerate() {
                control.check()?;
                if usize::try_from(*ordinal).ok() != Some(expected) {
                    return Err(invalid_ordinals());
                }
                crate::vector_index::validate_vector_values(dimensions, vector)?;
                values.push(vector.clone());
            }
            index.replace_controlled(document, values, Some(control))?;
            previous = Some(document);
        }
        Ok(Budgeted::new(index, memory))
    }

    pub(crate) fn prepare_canonical(
        dimensions: u32,
        params: IVFIndexParams,
        vectors: &[(DocId, u32, Vec<f32>)],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        let candidate = Self::from_canonical_controlled(dimensions, params, vectors, control)?;
        candidate.train_controlled(Some(control))?;
        candidate.metadata_controlled(control)
    }

    pub(crate) fn trained_snapshot_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let count = self.vectors.lock().len();
        let clusters = self.centroids.lock().len().max(self.nlist.min(count));
        let memory =
            control
                .memory()
                .reserve(workspace_bytes(self.dimensions, count, clusters)?)?;
        let candidate = self.clone_controlled(control)?;
        if candidate.state() == IVFState::Stale {
            candidate.train_controlled(Some(control))?;
        }
        Ok(Budgeted::new(candidate, memory))
    }
}

fn invalid_ordinals() -> StorageBackendError {
    StorageBackendError::Other("IVF canonical tensors require ordered complete ordinals".into())
}
