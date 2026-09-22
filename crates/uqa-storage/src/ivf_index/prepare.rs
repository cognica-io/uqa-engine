//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded preparation of evaluated IVF changes, independent of provider row encodings.

mod canonical;

use uqa_core::{
    memory::{Budgeted, MemoryError},
    DocId,
};

use super::{
    state::{StoredVector, VectorKey},
    IVFIndex, IVFMetadataSnapshot, IVFState,
};
use crate::{read_control::StorageReadControl, vector_index::IVFIndexParams, StorageBackendResult};

/// Already evaluated canonical values. Re-preparation never invokes SQL or application code. An empty replacement and deletion retain their distinct training-counter behavior.
#[derive(Clone, Copy)]
pub enum IVFMutation<'a> {
    Replace {
        document: DocId,
        vectors: &'a [Vec<f32>],
    },
    Delete(DocId),
    Clear,
    Train,
}

impl IVFIndex {
    /// Restore one validated physical generation under the caller's allowance. The returned index retains its reconstruction allowance until it is dropped.
    pub fn restore_controlled(
        dimensions: u32,
        params: IVFIndexParams,
        vectors: Vec<(DocId, u32, Vec<f32>)>,
        snapshot: IVFMetadataSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        params.validate()?;
        let mut bytes = workspace_bytes(dimensions, vectors.len(), snapshot.centroids.len())?;
        // Callers may transfer vectors with spare capacity; retain that actual allocation as well as reconstruction scratch.
        add_bytes(
            &mut bytes,
            checked_product(vectors.capacity(), size_of::<(DocId, u32, Vec<f32>)>())?,
        )?;
        add_bytes(
            &mut bytes,
            checked_product(snapshot.centroids.capacity(), size_of::<Vec<f32>>())?,
        )?;
        add_bytes(
            &mut bytes,
            checked_product(
                snapshot.assignments.capacity(),
                size_of::<(DocId, u32, usize)>(),
            )?,
        )?;
        for vector in vectors
            .iter()
            .map(|(_, _, vector)| vector)
            .chain(snapshot.centroids.iter())
        {
            control.check()?;
            add_bytes(
                &mut bytes,
                checked_product(vector.capacity(), size_of::<f32>())?,
            )?;
        }
        let memory = control.memory().reserve(bytes)?;
        let index = Self::restore(
            dimensions,
            params.nlist,
            params.nprobe,
            params.train_threshold,
            vectors,
            snapshot,
            Some(control),
        )?;
        Ok(Budgeted::new(index, memory))
    }

    /// Compute an immutable metadata candidate without changing the supplied index. All private state and numerical scratch are reserved before cloning; the published snapshot owns a separate allowance.
    pub fn prepare_metadata(
        &self,
        mutation: IVFMutation<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        self.prepare_metadata_changes(std::slice::from_ref(&mutation), control)
    }

    pub(crate) fn prepare_metadata_changes(
        &self,
        mutations: &[IVFMutation<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        control.check()?;
        if matches!(mutations, [IVFMutation::Clear]) {
            return Self::with_params(
                self.dimensions,
                self.nlist,
                self.nprobe(),
                self.train_threshold,
            )
            .metadata_controlled(control);
        }
        let mut count = self.vectors.lock().len();
        for mutation in mutations {
            if let IVFMutation::Replace { vectors, .. } = mutation {
                count = count
                    .checked_add(vectors.len())
                    .ok_or(MemoryError::SizeOverflow)?;
                for vector in *vectors {
                    control.check()?;
                    crate::vector_index::validate_vector_values(self.dimensions, vector)?;
                }
            }
        }
        let clusters = self.centroids.lock().len().max(self.nlist.min(count));
        let _workspace =
            control
                .memory()
                .reserve(workspace_bytes(self.dimensions, count, clusters)?)?;
        let mut candidate = self.clone_controlled(control)?;
        for mutation in mutations {
            control.check()?;
            match *mutation {
                IVFMutation::Replace { document, vectors } => {
                    let mut values = Vec::with_capacity(vectors.len());
                    for vector in vectors {
                        control.check()?;
                        values.push(vector.clone());
                    }
                    candidate.replace_controlled(document, values, Some(control))?;
                }
                IVFMutation::Delete(document) => {
                    candidate.delete_controlled(document, Some(control))?;
                }
                IVFMutation::Clear => candidate.clear_index(),
                IVFMutation::Train => candidate.train_controlled(Some(control))?,
            }
            if candidate.state() == IVFState::Stale {
                candidate.train_controlled(Some(control))?;
            }
        }
        candidate.metadata_controlled(control)
    }

    fn clone_controlled(&self, control: &StorageReadControl) -> StorageBackendResult<Self> {
        let candidate = Self::with_params(
            self.dimensions,
            self.nlist,
            self.nprobe(),
            self.train_threshold,
        );
        for (key, vector) in self.vectors.lock().iter() {
            control.check()?;
            candidate.vectors.lock().insert(*key, vector.clone());
        }
        let centroids = self.centroids.lock();
        *candidate.centroids.lock() = Vec::with_capacity(centroids.len());
        for centroid in centroids.iter() {
            control.check()?;
            candidate.centroids.lock().push(centroid.clone());
        }
        drop(centroids);
        let lists = self.inverted_lists.lock();
        *candidate.inverted_lists.lock() = Vec::with_capacity(lists.len());
        for list in lists.iter() {
            control.check()?;
            candidate.inverted_lists.lock().push(list.clone());
        }
        drop(lists);
        *candidate.state.lock() = self.state();
        *candidate.trained_size.lock() = *self.trained_size.lock();
        *candidate.deletes_since_train.lock() = *self.deletes_since_train.lock();
        Ok(candidate)
    }

    pub(crate) fn centroids_match(&self, snapshot: &IVFMetadataSnapshot) -> bool {
        *self.centroids.lock() == snapshot.centroids
    }

    fn metadata_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        let vectors = self.vectors.lock();
        let centroids = self.centroids.lock();
        let assignment_count = if self.state() == IVFState::Untrained {
            0
        } else {
            vectors.len()
        };
        let mut bytes = checked_product(assignment_count, size_of::<(DocId, u32, usize)>())?;
        add_bytes(
            &mut bytes,
            checked_product(centroids.len(), size_of::<Vec<f32>>())?,
        )?;
        for centroid in centroids.iter() {
            control.check()?;
            add_bytes(
                &mut bytes,
                checked_product(centroid.len(), size_of::<f32>())?,
            )?;
        }
        let memory = control.memory().reserve(bytes)?;
        let mut assignments = Vec::with_capacity(assignment_count);
        for vector in vectors.values() {
            control.check()?;
            if let Some(centroid) = vector.centroid {
                assignments.push((vector.doc_id, vector.vector_ordinal, centroid));
            }
        }
        let mut copied = Vec::with_capacity(centroids.len());
        for centroid in centroids.iter() {
            control.check()?;
            copied.push(centroid.clone());
        }
        Ok(Budgeted::new(
            IVFMetadataSnapshot {
                state: self.state(),
                centroids: copied,
                assignments,
                trained_size: *self.trained_size.lock(),
                deletes_since_train: *self.deletes_since_train.lock(),
                vector_count: vectors.len(),
            },
            memory,
        ))
    }
}

// Charge logical B-tree entries and every simultaneously live vector buffer. Allocator node bookkeeping is outside the payload allowance, as in common record preparation. Capacity growth is bounded separately from vector payloads.
fn workspace_bytes(dimensions: u32, count: usize, clusters: usize) -> Result<usize, MemoryError> {
    let coordinates = checked_product(
        usize::try_from(dimensions).map_err(|_| MemoryError::SizeOverflow)?,
        size_of::<f32>(),
    )?;
    // Stored raw/normalized values plus the training copy or evaluated replacement.
    let mut per_vector = checked_product(coordinates, 3)?;
    // Candidate B-tree, staged replacement, decoded inputs, assignment map and duplicate set.
    for size in [
        size_of::<(VectorKey, StoredVector)>(),
        size_of::<StoredVector>(),
        size_of::<(DocId, u32, Vec<f32>)>(),
        size_of::<(VectorKey, usize)>(),
        size_of::<VectorKey>(),
        size_of::<Vec<f32>>(),
    ] {
        add_bytes(&mut per_vector, size)?;
    }
    // Removal keys/results, insertion results, and geometric inverted-list capacity.
    for size in [
        size_of::<VectorKey>(),
        size_of::<(VectorKey, Option<usize>)>(),
        size_of::<(VectorKey, usize)>(),
        4 * size_of::<VectorKey>(),
    ] {
        add_bytes(&mut per_vector, size)?;
    }
    let mut bytes = checked_product(count, per_vector)?;
    // Existing centroids, new centroids and k-means sums, including geometric clone capacity; counts and inverted-list headers/minimum capacities coexist.
    let mut per_cluster = checked_product(coordinates, 3)?;
    add_bytes(
        &mut per_cluster,
        4 * size_of::<Vec<f32>>()
            + size_of::<usize>()
            + 2 * size_of::<Vec<VectorKey>>()
            + 8 * size_of::<VectorKey>(),
    )?;
    add_bytes(&mut bytes, checked_product(clusters, per_cluster)?)?;
    Ok(bytes)
}

fn checked_product(left: usize, right: usize) -> Result<usize, MemoryError> {
    left.checked_mul(right).ok_or(MemoryError::SizeOverflow)
}

fn add_bytes(total: &mut usize, value: usize) -> Result<(), MemoryError> {
    *total = total.checked_add(value).ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}

pub(super) fn check(control: Option<&StorageReadControl>) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}
