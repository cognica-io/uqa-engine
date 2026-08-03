//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked conversion between `SQLite` scalar metadata and HNSW state.

use crate::hnsw_index::{HNSWGraphMeta, MAX_HNSW_LEVEL};
use crate::sqlite::{Result as SQLiteResult, SQLiteError};
use crate::vector_index::HNSWIndexParams;

pub(super) const HNSW_FORMAT_VERSION: i64 = 1;

pub(super) type RawMeta = (
    i64,
    i64,
    i64,
    i64,
    i64,
    String,
    Option<i64>,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
);

pub(super) fn decode_meta(
    raw: RawMeta,
) -> SQLiteResult<(u32, HNSWIndexParams, HNSWGraphMeta, u64)> {
    let (
        dimensions,
        m,
        ef_construction,
        ef_search,
        rebuild_threshold,
        seed,
        entry_point,
        max_level,
        next_node_id,
        live_count,
        deleted_count,
        revision,
        format_version,
    ) = raw;
    if format_version != HNSW_FORMAT_VERSION {
        return Err(invalid_metadata(
            "format_version",
            &format_version.to_string(),
        ));
    }
    let params = HNSWIndexParams {
        m: checked_positive_usize("m", m)?,
        ef_construction: checked_positive_usize("ef_construction", ef_construction)?,
        ef_search: checked_positive_usize("ef_search", ef_search)?,
        rebuild_threshold: checked_positive_usize("rebuild_threshold", rebuild_threshold)?,
        seed: seed
            .parse::<u64>()
            .map_err(|_| invalid_metadata("seed", &seed))?,
    };
    params
        .validate()
        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    Ok((
        u32::try_from(dimensions)
            .map_err(|_| invalid_metadata("dimensions", &dimensions.to_string()))?,
        params,
        HNSWGraphMeta {
            entry_point: entry_point
                .map(|value| checked_u64("entry_node_id", value))
                .transpose()?,
            max_level: checked_hnsw_level("max_level", max_level)?,
            next_node_id: checked_u64("next_node_id", next_node_id)?,
            live_count: checked_usize("live_count", live_count)?,
            deleted_count: checked_usize("deleted_count", deleted_count)?,
        },
        checked_u64("revision", revision)?,
    ))
}

fn checked_positive_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    let value = checked_usize(field, value)?;
    if value == 0 {
        Err(invalid_metadata(field, "0"))
    } else {
        Ok(value)
    }
}

pub(super) fn checked_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    usize::try_from(value).map_err(|_| invalid_metadata(field, &value.to_string()))
}

pub(super) fn checked_hnsw_level(field: &str, value: i64) -> SQLiteResult<usize> {
    let level = checked_usize(field, value)?;
    if level > MAX_HNSW_LEVEL {
        return Err(SQLiteError::StorageBackend(format!(
            "invalid HNSW metadata {field}: {level} exceeds the supported maximum {MAX_HNSW_LEVEL}"
        )));
    }
    Ok(level)
}

pub(super) fn checked_u64(field: &str, value: i64) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| invalid_metadata(field, &value.to_string()))
}

pub(super) fn checked_i64(field: &str, value: usize) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| invalid_metadata(field, &value.to_string()))
}

pub(super) fn checked_i64_u64(field: &str, value: u64) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| invalid_metadata(field, &value.to_string()))
}

pub(super) fn invalid_metadata(field: &str, value: &str) -> SQLiteError {
    SQLiteError::StorageBackend(format!("invalid HNSW metadata {field}: {value}"))
}
