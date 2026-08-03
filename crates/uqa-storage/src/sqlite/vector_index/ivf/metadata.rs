//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked IVF metadata representation, encoding, and scalar conversion.

use crate::ivf_index::{IVFMetadataSnapshot, IVFState};
use crate::sqlite::vector_index::codec::{encode_doc_id, vector_to_blob};
use crate::sqlite::{Result as SQLiteResult, SQLiteError};
use crate::vector_index::IVFIndexParams;

#[derive(Debug, Clone)]
pub(super) struct SQLiteIVFMeta {
    pub(super) dimensions: u32,
    pub(super) params: IVFIndexParams,
    pub(super) state: IVFState,
    pub(super) vector_count: usize,
}

pub(super) struct EncodedIVFMetadata {
    pub(super) nlist: i64,
    pub(super) nprobe: i64,
    pub(super) train_threshold: i64,
    pub(super) state: IVFState,
    pub(super) trained_size: i64,
    pub(super) deletes_since_train: i64,
    pub(super) vector_count: i64,
    pub(super) centroids: Vec<(i64, Vec<u8>)>,
    pub(super) assignments: Vec<(i64, i64, i64)>,
}

pub(super) fn encode_metadata(
    params: IVFIndexParams,
    snapshot: &IVFMetadataSnapshot,
) -> SQLiteResult<EncodedIVFMetadata> {
    let centroids = snapshot
        .centroids
        .iter()
        .enumerate()
        .map(|(centroid_id, centroid)| {
            Ok((
                usize_to_i64("centroid_id", centroid_id)?,
                vector_to_blob(centroid)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    let assignments = snapshot
        .assignments
        .iter()
        .map(|(doc_id, ordinal, centroid)| {
            Ok((
                encode_doc_id(*doc_id)?,
                i64::from(*ordinal),
                usize_to_i64("centroid_id", *centroid)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    Ok(EncodedIVFMetadata {
        nlist: usize_to_i64("nlist", params.nlist)?,
        nprobe: usize_to_i64("nprobe", params.nprobe)?,
        train_threshold: usize_to_i64("train_threshold", params.train_threshold)?,
        state: snapshot.state,
        trained_size: usize_to_i64("trained_size", snapshot.trained_size)?,
        deletes_since_train: usize_to_i64("deletes_since_train", snapshot.deletes_since_train)?,
        vector_count: usize_to_i64("vector_count", snapshot.vector_count)?,
        centroids,
        assignments,
    })
}

pub(super) fn invalid_metadata(field: &str, value: i64) -> SQLiteError {
    SQLiteError::StorageBackend(format!("invalid IVF metadata {field}: {value}"))
}

pub(super) fn positive_i64_to_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    let value = usize::try_from(value).map_err(|_| invalid_metadata(field, value))?;
    if value == 0 {
        Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata {field}: expected a positive value"
        )))
    } else {
        Ok(value)
    }
}

pub(super) fn usize_to_i64(field: &str, value: usize) -> SQLiteResult<i64> {
    value.try_into().map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "IVF metadata {field} does not fit in SQLite INTEGER"
        ))
    })
}

pub(super) fn state_to_str(state: IVFState) -> &'static str {
    match state {
        IVFState::Untrained => "untrained",
        IVFState::Trained => "trained",
        IVFState::Stale => "stale",
    }
}

pub(super) fn parse_state(value: &str) -> SQLiteResult<IVFState> {
    match value {
        "untrained" => Ok(IVFState::Untrained),
        "trained" => Ok(IVFState::Trained),
        "stale" => Ok(IVFState::Stale),
        other => Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata state: {other}"
        ))),
    }
}
