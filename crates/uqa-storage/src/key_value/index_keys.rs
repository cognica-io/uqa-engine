//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered key layouts for durable scalar and vector physical indexes.

use uqa_core::DocId;

use super::codec::{push_str, push_u64, table_prefixed_key, usize_to_u64};
use super::{
    StorageBackendResult, TAG_BTREE_ENTRY, TAG_BTREE_INDEX, TAG_HNSW_METADATA, TAG_HNSW_NODE,
    TAG_IVF_ASSIGNMENT, TAG_IVF_CENTROID, TAG_IVF_METADATA,
};

pub(super) fn btree_index_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_BTREE_INDEX, table)
}

pub(super) fn btree_entry_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_BTREE_ENTRY, table)
}

pub(super) fn btree_index_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = btree_index_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn btree_entry_field_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = btree_entry_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn btree_entry_key(
    table: &str,
    field: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = btree_entry_field_prefix(table, field)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn vector_physical_field_key(tag: u8, table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_physical_table_prefix(tag, table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn vector_physical_table_prefix(tag: u8, table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(tag, table)
}

pub(super) fn ivf_metadata_table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_table_prefix(TAG_IVF_METADATA, table)
}

pub(super) fn ivf_metadata_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_field_key(TAG_IVF_METADATA, table, field)
}

pub(super) fn ivf_centroid_table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_table_prefix(TAG_IVF_CENTROID, table)
}

pub(super) fn ivf_centroid_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_field_key(TAG_IVF_CENTROID, table, field)
}

pub(super) fn ivf_centroid_key(
    table: &str,
    field: &str,
    centroid: usize,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = ivf_centroid_prefix(table, field)?;
    push_u64(&mut key, usize_to_u64(centroid, "IVF centroid id")?);
    Ok(key)
}

pub(super) fn ivf_assignment_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_field_key(TAG_IVF_ASSIGNMENT, table, field)
}

pub(super) fn ivf_assignment_table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_table_prefix(TAG_IVF_ASSIGNMENT, table)
}

pub(super) fn ivf_assignment_doc_prefix(
    table: &str,
    field: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = ivf_assignment_prefix(table, field)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn ivf_assignment_key(
    table: &str,
    field: &str,
    doc_id: DocId,
    ordinal: u32,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = ivf_assignment_doc_prefix(table, field, doc_id)?;
    push_u64(&mut key, u64::from(ordinal));
    Ok(key)
}

pub(super) fn hnsw_metadata_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_field_key(TAG_HNSW_METADATA, table, field)
}

pub(super) fn hnsw_metadata_table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_table_prefix(TAG_HNSW_METADATA, table)
}

pub(super) fn hnsw_node_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_field_key(TAG_HNSW_NODE, table, field)
}

pub(super) fn hnsw_node_table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    vector_physical_table_prefix(TAG_HNSW_NODE, table)
}

pub(super) fn hnsw_node_key(
    table: &str,
    field: &str,
    node_id: u64,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = hnsw_node_prefix(table, field)?;
    push_u64(&mut key, node_id);
    Ok(key)
}
