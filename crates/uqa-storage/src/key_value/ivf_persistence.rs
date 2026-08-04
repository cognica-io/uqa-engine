//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned IVF metadata encoding, restoration, and atomic snapshot writes.

use serde::{Deserialize, Serialize};
use uqa_core::DocId;

use super::codec::{
    blob_to_vector, decode_u64_value, decode_value, encode_value, other_error, read_u64,
    usize_to_u64, vector_to_blob,
};
use super::index_keys::{
    ivf_assignment_doc_prefix, ivf_assignment_key, ivf_assignment_prefix, ivf_centroid_key,
    ivf_centroid_prefix, ivf_metadata_key,
};
use super::{KeyValueBatch, KeyValueStore, KeyValueVectorIndex};
use crate::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState};
use crate::vector_index::IVFIndexParams;
use crate::StorageBackendResult;

const IVF_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedIVFMetadata {
    format_version: u32,
    dimensions: u32,
    nlist: u64,
    nprobe: u64,
    train_threshold: u64,
    state: PersistedIVFState,
    trained_size: u64,
    deletes_since_train: u64,
    vector_count: u64,
    revision: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedIVFState {
    Untrained,
    Trained,
    Stale,
}

pub(super) fn restore_state(
    store: &dyn KeyValueStore,
    raw: &KeyValueVectorIndex,
    table: &str,
    field: &str,
    dimensions: u32,
    params: IVFIndexParams,
) -> StorageBackendResult<(IVFIndex, u64)> {
    let metadata = load_metadata(store, table, field)?.ok_or_else(|| {
        other_error(format!(
            "missing persisted IVF metadata for {table}.{field}"
        ))
    })?;
    validate_metadata(&metadata, table, field, dimensions, params)?;
    let snapshot = IVFMetadataSnapshot {
        state: metadata.state.into(),
        centroids: load_centroids(store, table, field)?,
        assignments: load_assignments(store, table, field)?,
        trained_size: checked_usize(metadata.trained_size, "IVF trained_size")?,
        deletes_since_train: checked_usize(
            metadata.deletes_since_train,
            "IVF deletes_since_train",
        )?,
        vector_count: checked_usize(metadata.vector_count, "IVF vector_count")?,
    };
    let index = IVFIndex::from_persistence(
        dimensions,
        params.nlist,
        params.nprobe,
        params.train_threshold,
        raw.load_all_with_ordinals()?,
        snapshot,
    )?;
    Ok((index, metadata.revision))
}

pub(super) fn load_revision(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<u64>> {
    Ok(load_metadata(store, table, field)?.map(|metadata| metadata.revision))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_snapshot(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
    dimensions: u32,
    params: IVFIndexParams,
    snapshot: &IVFMetadataSnapshot,
    revision: u64,
    full_rewrite: bool,
    changed_doc: Option<DocId>,
) -> StorageBackendResult<()> {
    batch.put(
        &ivf_metadata_key(table, field)?,
        &encode_value(&metadata_from_snapshot(
            dimensions, params, snapshot, revision,
        )?)?,
    )?;
    if full_rewrite {
        batch.delete_prefix(&ivf_centroid_prefix(table, field)?)?;
        batch.delete_prefix(&ivf_assignment_prefix(table, field)?)?;
        for (centroid, vector) in snapshot.centroids.iter().enumerate() {
            batch.put(
                &ivf_centroid_key(table, field, centroid)?,
                &vector_to_blob(vector)?,
            )?;
        }
        for (doc_id, ordinal, centroid) in &snapshot.assignments {
            put_assignment(batch, table, field, *doc_id, *ordinal, *centroid)?;
        }
    } else if let Some(doc_id) = changed_doc {
        batch.delete_prefix(&ivf_assignment_doc_prefix(table, field, doc_id)?)?;
        for (_, ordinal, centroid) in snapshot
            .assignments
            .iter()
            .filter(|(candidate, _, _)| *candidate == doc_id)
        {
            put_assignment(batch, table, field, doc_id, *ordinal, *centroid)?;
        }
    }
    Ok(())
}

fn put_assignment(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
    doc_id: DocId,
    ordinal: u32,
    centroid: usize,
) -> StorageBackendResult<()> {
    batch.put(
        &ivf_assignment_key(table, field, doc_id, ordinal)?,
        &usize_to_u64(centroid, "IVF centroid assignment")?.to_be_bytes(),
    )
}

fn load_metadata(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<PersistedIVFMetadata>> {
    store
        .get(&ivf_metadata_key(table, field)?)?
        .map(|bytes| decode_value(&bytes))
        .transpose()
}

fn load_centroids(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Vec<Vec<f32>>> {
    let prefix = ivf_centroid_prefix(table, field)?;
    let mut centroids = Vec::new();
    for (expected, (key, value)) in store.scan_prefix(&prefix)?.into_iter().enumerate() {
        let mut offset = prefix.len();
        let found = read_u64(&key, &mut offset)?;
        if offset != key.len() || found != usize_to_u64(expected, "IVF centroid id")? {
            return Err(other_error("corrupt IVF centroid key sequence"));
        }
        centroids.push(blob_to_vector(&value)?);
    }
    Ok(centroids)
}

fn load_assignments(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Vec<(DocId, u32, usize)>> {
    let prefix = ivf_assignment_prefix(table, field)?;
    let mut assignments = Vec::new();
    for (key, value) in store.scan_prefix(&prefix)? {
        let mut offset = prefix.len();
        let doc_id = read_u64(&key, &mut offset)?;
        let ordinal = u32::try_from(read_u64(&key, &mut offset)?)
            .map_err(|_| other_error("persisted IVF ordinal exceeds u32"))?;
        if offset != key.len() {
            return Err(other_error(
                "persisted IVF assignment key has trailing bytes",
            ));
        }
        let centroid = checked_usize(decode_u64_value(&value)?, "IVF centroid assignment")?;
        assignments.push((doc_id, ordinal, centroid));
    }
    Ok(assignments)
}

fn metadata_from_snapshot(
    dimensions: u32,
    params: IVFIndexParams,
    snapshot: &IVFMetadataSnapshot,
    revision: u64,
) -> StorageBackendResult<PersistedIVFMetadata> {
    Ok(PersistedIVFMetadata {
        format_version: IVF_FORMAT_VERSION,
        dimensions,
        nlist: usize_to_u64(params.nlist, "IVF nlist")?,
        nprobe: usize_to_u64(params.nprobe, "IVF nprobe")?,
        train_threshold: usize_to_u64(params.train_threshold, "IVF train_threshold")?,
        state: snapshot.state.into(),
        trained_size: usize_to_u64(snapshot.trained_size, "IVF trained_size")?,
        deletes_since_train: usize_to_u64(snapshot.deletes_since_train, "IVF deletes_since_train")?,
        vector_count: usize_to_u64(snapshot.vector_count, "IVF vector_count")?,
        revision,
    })
}

fn validate_metadata(
    metadata: &PersistedIVFMetadata,
    table: &str,
    field: &str,
    dimensions: u32,
    params: IVFIndexParams,
) -> StorageBackendResult<()> {
    let persisted_params = IVFIndexParams {
        nlist: checked_usize(metadata.nlist, "IVF nlist")?,
        nprobe: checked_usize(metadata.nprobe, "IVF nprobe")?,
        train_threshold: checked_usize(metadata.train_threshold, "IVF train_threshold")?,
    };
    if metadata.format_version != IVF_FORMAT_VERSION
        || metadata.dimensions != dimensions
        || persisted_params != params
    {
        return Err(other_error(format!(
            "persisted IVF metadata does not match the catalog for {table}.{field}"
        )));
    }
    Ok(())
}

fn checked_usize(value: u64, field: &str) -> StorageBackendResult<usize> {
    usize::try_from(value).map_err(|_| other_error(format!("{field} exceeds usize")))
}

impl From<IVFState> for PersistedIVFState {
    fn from(value: IVFState) -> Self {
        match value {
            IVFState::Untrained => Self::Untrained,
            IVFState::Trained => Self::Trained,
            IVFState::Stale => Self::Stale,
        }
    }
}

impl From<PersistedIVFState> for IVFState {
    fn from(value: PersistedIVFState) -> Self {
        match value {
            PersistedIVFState::Untrained => Self::Untrained,
            PersistedIVFState::Trained => Self::Trained,
            PersistedIVFState::Stale => Self::Stale,
        }
    }
}
