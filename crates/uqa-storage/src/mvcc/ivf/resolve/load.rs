//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode one committed IVF generation while retaining every payload allowance.

use crate::mvcc::{
    CommittedRecordSnapshot, IVFRecordHeader, IVFRecordKey as Key, IVFRecordLayout, VersionError,
    VersionResult,
};
use crate::{
    ivf_index::{IVFIndex, IVFMetadataSnapshot},
    read_control::StorageReadControl,
};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

pub(super) fn index(
    key: &[u8],
    header: IVFRecordHeader,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<Budgeted<IVFIndex>> {
    let mut payload = control.memory().reserve(0)?;
    let mut centroids = BudgetedVec::<Vec<f32>>::new(control.memory());
    let mut assignments = BudgetedVec::<(DocId, u32, usize)>::new(control.memory());
    let mut vectors = BudgetedVec::<(DocId, u32, Vec<f32>)>::new(control.memory());
    current.visit_prefix(
        &layout.key(key, Key::Centroids, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (id, vector) = layout.centroid(key, value, control)?;
                if id != centroids.len() {
                    return Err(VersionError::InvalidEncoding(
                        "invalid IVF centroid sequence",
                    ));
                }
                centroids.reserve(1)?;
                let (vector, memory) = vector.into_parts();
                payload.absorb(memory);
                centroids.push(vector)?;
            }
            Ok(true)
        },
    )?;
    current.visit_prefix(
        &layout.key(key, Key::Assignments, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                assignments.push(layout.assignment(key, value, control)?)?;
            }
            Ok(true)
        },
    )?;
    current.visit_prefix(
        &layout.key(key, Key::Vectors, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (document, ordinal, vector) = layout.vector(key, value, control)?;
                vectors.reserve(1)?;
                let (vector, memory) = vector.into_parts();
                payload.absorb(memory);
                vectors.push((document, ordinal, vector))?;
            }
            Ok(true)
        },
    )?;
    let (centroids, centroid_memory) = centroids.into_parts();
    let (assignments, assignment_memory) = assignments.into_parts();
    let (vectors, vector_memory) = vectors.into_parts();
    let snapshot = IVFMetadataSnapshot {
        state: header.state,
        centroids,
        assignments,
        trained_size: header.trained_size,
        deletes_since_train: header.deletes_since_train,
        vector_count: header.vector_count,
    };
    let index =
        IVFIndex::restore_controlled(header.dimensions, header.params, vectors, snapshot, control)?;
    drop((payload, centroid_memory, assignment_memory, vector_memory));
    Ok(index)
}
