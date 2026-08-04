//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned K/V physical-index namespaces for table and column lifecycle changes.

use super::keys::{batch_rekey_prefix, batch_rekey_prefix_or_keep_existing};
use crate::key_value::index_keys::{
    btree_entry_field_prefix, btree_entry_key_prefix, btree_index_key, btree_index_key_prefix,
    hnsw_metadata_key, hnsw_metadata_table_prefix, hnsw_node_prefix, hnsw_node_table_prefix,
    ivf_assignment_prefix, ivf_assignment_table_prefix, ivf_centroid_prefix,
    ivf_centroid_table_prefix, ivf_metadata_key, ivf_metadata_table_prefix,
};
use crate::key_value::{KeyValueBatch, KeyValueStore};
use crate::StorageBackendResult;

pub(super) fn drop_table_indexes(
    batch: &mut dyn KeyValueBatch,
    table: &str,
) -> StorageBackendResult<()> {
    for prefix in table_index_prefixes(table)? {
        batch.delete_prefix(&prefix)?;
    }
    Ok(())
}

pub(super) fn rename_table_indexes(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    for (old_prefix, new_prefix) in table_index_prefixes(from)?
        .into_iter()
        .zip(table_index_prefixes(to)?)
    {
        batch_rekey_prefix(store, batch, &old_prefix, &new_prefix)?;
    }
    Ok(())
}

pub(super) fn drop_field_indexes(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
) -> StorageBackendResult<()> {
    batch.delete(&btree_index_key(table, field)?)?;
    for prefix in field_index_prefixes(table, field)? {
        batch.delete_prefix(&prefix)?;
    }
    Ok(())
}

pub(super) fn rename_field_indexes(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    batch_rekey_prefix_or_keep_existing(
        store,
        batch,
        &btree_index_key(table, from)?,
        &btree_index_key(table, to)?,
    )?;
    for (old_prefix, new_prefix) in field_index_prefixes(table, from)?
        .into_iter()
        .zip(field_index_prefixes(table, to)?)
    {
        batch_rekey_prefix_or_keep_existing(store, batch, &old_prefix, &new_prefix)?;
    }
    Ok(())
}

pub(super) fn table_index_prefixes(table: &str) -> StorageBackendResult<[Vec<u8>; 7]> {
    Ok([
        btree_index_key_prefix(table)?,
        btree_entry_key_prefix(table)?,
        ivf_metadata_table_prefix(table)?,
        ivf_centroid_table_prefix(table)?,
        ivf_assignment_table_prefix(table)?,
        hnsw_metadata_table_prefix(table)?,
        hnsw_node_table_prefix(table)?,
    ])
}

fn field_index_prefixes(table: &str, field: &str) -> StorageBackendResult<[Vec<u8>; 6]> {
    Ok([
        btree_entry_field_prefix(table, field)?,
        ivf_metadata_key(table, field)?,
        ivf_centroid_prefix(table, field)?,
        ivf_assignment_prefix(table, field)?,
        hnsw_metadata_key(table, field)?,
        hnsw_node_prefix(table, field)?,
    ])
}
