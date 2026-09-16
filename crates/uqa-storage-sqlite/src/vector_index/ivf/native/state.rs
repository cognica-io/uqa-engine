//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row decoding and encoding for common IVF candidate preparation.

use uqa_core::memory::{Budgeted, MemoryError};
use uqa_storage::{
    ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState},
    read_control::StorageReadControl,
    IVFIndexParams,
};

use super::super::metadata::{encode_metadata, EncodedIVFMetadata};
use super::{load_centroids, load_metadata};
use crate::{
    mvcc::native::NativeRecordFamily as Family,
    vector_index::{
        decode_doc_id,
        native::{integer, NativeVectorRead, VectorBuffer},
    },
    Result, SQLiteError,
};

pub(in crate::vector_index::ivf) fn load_state(
    read: &NativeVectorRead<'_>,
    params: IVFIndexParams,
    rebuild: bool,
) -> Result<Budgeted<IVFIndex>> {
    let vectors = read.vectors()?;
    let mut snapshot = IVFMetadataSnapshot {
        state: IVFState::Untrained,
        centroids: Vec::new(),
        assignments: Vec::new(),
        trained_size: 0,
        deletes_since_train: 0,
        vector_count: vectors.len(),
    };
    // Decoded rows and their leases remain paired while common storage validates and takes ownership of the values.
    let mut decoded = None;
    if !rebuild {
        let meta = load_metadata(read)?;
        if let Some(meta) = meta {
            if meta.dimensions != read.index.dimensions || meta.params != params {
                return Err(SQLiteError::StorageBackend(
                    "native IVF definition does not match its handle".into(),
                ));
            }
            snapshot.state = meta.state;
            snapshot.trained_size = meta.trained_size;
            snapshot.deletes_since_train = meta.deletes_since_train;
            snapshot.vector_count = meta.vector_count;
        } else if !vectors.is_empty() {
            return Err(SQLiteError::StorageBackend(
                "missing native IVF metadata".into(),
            ));
        }
        let centroids = load_centroids(read)?;
        let mut assignments = VectorBuffer::new(read)?;
        if let Some(owner) = read.owner {
            read.snapshot.visit_rows(
                Family::IVFAssignments,
                Some(owner),
                &[read.field()],
                |row| {
                    assignments.rows.push((
                        decode_doc_id(integer(row[2])?)?,
                        u32::try_from(integer(row[3])?).map_err(|_| {
                            SQLiteError::StorageBackend("invalid native IVF ordinal".into())
                        })?,
                        usize::try_from(integer(row[4])?).map_err(|_| {
                            SQLiteError::StorageBackend("invalid native IVF centroid".into())
                        })?,
                    ))?;
                    Ok(())
                },
            )?;
        }
        let (centroids, centroid_memory) = centroids.into_parts();
        let (assignments, assignment_memory) = assignments.finish().into_parts();
        snapshot.centroids = centroids;
        snapshot.assignments = assignments;
        decoded = Some((centroid_memory, assignment_memory));
    }
    let (vectors, vector_memory) = vectors.into_parts();
    let index = IVFIndex::restore_controlled(
        read.index.dimensions,
        params,
        vectors,
        snapshot,
        &read.snapshot.control,
    )?;
    drop((decoded, vector_memory));
    Ok(index)
}

pub(in crate::vector_index::ivf) fn encode_controlled(
    params: IVFIndexParams,
    snapshot: &IVFMetadataSnapshot,
    control: &StorageReadControl,
) -> Result<Budgeted<EncodedIVFMetadata>> {
    control.check()?;
    let mut bytes = snapshot
        .assignments
        .len()
        .checked_mul(size_of::<(i64, i64, i64)>())
        .ok_or(MemoryError::SizeOverflow)?;
    bytes = bytes
        .checked_add(
            snapshot
                .centroids
                .len()
                .checked_mul(size_of::<(i64, Vec<u8>)>())
                .ok_or(MemoryError::SizeOverflow)?,
        )
        .ok_or(MemoryError::SizeOverflow)?;
    for centroid in &snapshot.centroids {
        control.check()?;
        bytes = bytes
            .checked_add(
                centroid
                    .len()
                    .checked_mul(size_of::<f32>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
    }
    let memory = control.memory().reserve(bytes)?;
    let value = encode_metadata(params, snapshot)?;
    control.check()?;
    Ok(Budgeted::new(value, memory))
}
