//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned K/V physical-index namespaces for table and column lifecycle changes.

use super::keys::{
    batch_rekey_prefix_or_keep_existing, catalog_index_references_column, rekey_prefix,
};
use crate::key_value::codec::vector_field_prefix;
use crate::key_value::index_keys::{
    btree_entry_field_prefix, btree_entry_key_prefix, btree_index_key, btree_index_key_prefix,
    hnsw_metadata_key, hnsw_metadata_table_prefix, hnsw_node_prefix, hnsw_node_table_prefix,
    ivf_assignment_prefix, ivf_assignment_table_prefix, ivf_centroid_prefix,
    ivf_centroid_table_prefix, ivf_metadata_key, ivf_metadata_table_prefix,
};
use crate::key_value::vector_index::origin;
use crate::key_value::{KeyValueBatch, KeyValueRead, KeyValueStore};
use crate::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

fn vector_data_prefixes(table: &str, field: &str) -> StorageBackendResult<[Vec<u8>; 5]> {
    let [populations, witnesses] = origin::populations::field_prefixes(table, field)?;
    Ok([
        vector_field_prefix(table, field)?,
        origin::prefix(table, field)?,
        origin::journal::prefix(table, field)?,
        populations,
        witnesses,
    ])
}

fn has_vector_records(read: &dyn KeyValueRead, prefixes: &[Vec<u8>]) -> StorageBackendResult<bool> {
    for prefix in prefixes {
        let mut occupied = false;
        read.visit_keys_after(prefix, None, 1, read.control(), &mut |_| {
            occupied = true;
            Ok(())
        })?;
        if occupied {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Ordinary IVF/HNSW column rewrites can backfill raw destination values before moving field storage. A DiskANN-owned source or destination must never merge those independent canonical populations.
pub(super) fn reject_diskann_field_merge(
    store: &dyn KeyValueStore,
    table: &str,
    from: &str,
    to: &str,
    indexes: &[CatalogIndexRow],
) -> StorageBackendResult<()> {
    let source = vector_data_prefixes(table, from)?;
    let destination = vector_data_prefixes(table, to)?;
    store.with_read_view(&mut |read| {
        if !has_vector_records(read, &destination)? {
            return Ok(());
        }
        let mut diskann =
            has_vector_records(read, &source[1..])? || has_vector_records(read, &destination[1..])?;
        for index in indexes {
            if index.table_name == table && index.index_type.eq_ignore_ascii_case("diskann") {
                diskann |= catalog_index_references_column(index, from)?
                    || catalog_index_references_column(index, to)?;
            }
        }
        if diskann {
            return Err(StorageBackendError::Other(
                "column rename would merge DiskANN canonical fields".into(),
            ));
        }
        Ok(())
    })
}

pub(super) fn rename_vector_data(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    for (old, new) in vector_data_prefixes(table, from)?
        .into_iter()
        .zip(vector_data_prefixes(table, to)?)
    {
        batch_rekey_prefix_or_keep_existing(store, batch, &old, &new)?;
    }
    Ok(())
}

pub(super) fn drop_table_indexes(
    batch: &mut dyn KeyValueBatch,
    table: &str,
) -> StorageBackendResult<()> {
    batch.fence_ivf_prefix(&ivf_metadata_table_prefix(table)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_table_prefix(table)?)?;
    for prefix in table_index_prefixes(table)? {
        batch.delete_prefix(&prefix)?;
    }
    Ok(())
}

pub(super) fn rename_table_indexes(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    batch.fence_ivf_prefix(&ivf_metadata_table_prefix(from)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_table_prefix(from)?)?;
    batch.fence_ivf_prefix(&ivf_metadata_table_prefix(to)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_table_prefix(to)?)?;
    for (old_prefix, new_prefix) in table_index_prefixes(from)?
        .into_iter()
        .zip(table_index_prefixes(to)?)
    {
        rekey_prefix(read, batch, &old_prefix, &new_prefix)?;
    }
    Ok(())
}

pub(super) fn drop_field_indexes(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
) -> StorageBackendResult<()> {
    batch.fence_ivf_prefix(&ivf_metadata_key(table, field)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_key(table, field)?)?;
    batch.delete(&btree_index_key(table, &field.into())?)?;
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
    batch.fence_ivf_prefix(&ivf_metadata_key(table, from)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_key(table, from)?)?;
    batch.fence_ivf_prefix(&ivf_metadata_key(table, to)?)?;
    batch.fence_hnsw_prefix(&hnsw_metadata_key(table, to)?)?;
    batch_rekey_prefix_or_keep_existing(
        store,
        batch,
        &btree_index_key(table, &from.into())?,
        &btree_index_key(table, &to.into())?,
    )?;
    for (old_prefix, new_prefix) in field_index_prefixes(table, from)?
        .into_iter()
        .zip(field_index_prefixes(table, to)?)
    {
        batch_rekey_prefix_or_keep_existing(store, batch, &old_prefix, &new_prefix)?;
    }
    Ok(())
}

pub(super) fn table_index_prefixes(table: &str) -> StorageBackendResult<[Vec<u8>; 9]> {
    Ok([
        btree_index_key_prefix(table)?,
        super::super::codec::table_prefixed_key(super::super::TAG_NAMED_BTREE_INDEX, table)?,
        super::super::codec::table_prefixed_key(super::super::TAG_NAMED_BTREE_ENTRY, table)?,
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
        btree_entry_field_prefix(table, &field.into())?,
        ivf_metadata_key(table, field)?,
        ivf_centroid_prefix(table, field)?,
        ivf_assignment_prefix(table, field)?,
        hnsw_metadata_key(table, field)?,
        hnsw_node_prefix(table, field)?,
    ])
}
