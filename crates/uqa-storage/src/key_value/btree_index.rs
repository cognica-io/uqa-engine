//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable B-tree posting definitions and values over ordered byte keys.

use std::collections::BTreeMap;

use uqa_core::{DocId, Value};

use super::codec::{decode_value, encode_value, other_error, read_str, read_u64};
use super::index_keys::{
    btree_entry_field_prefix, btree_entry_key, btree_index_key, btree_index_key_prefix,
};
use super::{KeyValueStore, StorageBackendResult};

const BTREE_FORMAT_V1: &[u8] = b"uqa-btree-v1";

pub(super) fn load(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
    let Some(format) = store.get(&btree_index_key(table, field)?)? else {
        return Ok(None);
    };
    if format != BTREE_FORMAT_V1 {
        return Err(other_error(format!(
            "unsupported persisted B-tree format for `{table}.{field}`"
        )));
    }
    let prefix = btree_entry_field_prefix(table, field)?;
    let mut entries = Vec::new();
    for (key, value) in store.scan_prefix(&prefix)? {
        let mut offset = prefix.len();
        let doc_id = read_u64(&key, &mut offset)?;
        if offset != key.len() {
            return Err(other_error("persisted B-tree entry key has trailing bytes"));
        }
        entries.push((doc_id, decode_value(&value)?));
    }
    Ok(Some(entries))
}

pub(super) fn fields(store: &dyn KeyValueStore, table: &str) -> StorageBackendResult<Vec<String>> {
    let prefix = btree_index_key_prefix(table)?;
    let mut fields = Vec::new();
    for (key, value) in store.scan_prefix(&prefix)? {
        if value != BTREE_FORMAT_V1 {
            return Err(other_error(format!(
                "unsupported persisted B-tree format for table `{table}`"
            )));
        }
        let mut offset = prefix.len();
        let field = read_str(&key, &mut offset)?;
        if offset != key.len() {
            return Err(other_error(
                "persisted B-tree definition key has trailing bytes",
            ));
        }
        fields.push(field);
    }
    Ok(fields)
}

pub(super) fn replace(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
    values: &[(DocId, Value)],
) -> StorageBackendResult<()> {
    let mut batch = store.batch();
    batch.put(&btree_index_key(table, field)?, BTREE_FORMAT_V1)?;
    batch.delete_prefix(&btree_entry_field_prefix(table, field)?)?;
    for (doc_id, value) in values {
        batch.put(
            &btree_entry_key(table, field, *doc_id)?,
            &encode_value(value)?,
        )?;
    }
    batch.commit()
}

pub(super) fn replace_many(
    store: &dyn KeyValueStore,
    table: &str,
    indexes: &[(&str, &[(DocId, Value)])],
) -> StorageBackendResult<()> {
    let mut batch = store.batch();
    for (field, values) in indexes {
        batch.put(&btree_index_key(table, field)?, BTREE_FORMAT_V1)?;
        batch.delete_prefix(&btree_entry_field_prefix(table, field)?)?;
        for (doc_id, value) in *values {
            batch.put(
                &btree_entry_key(table, field, *doc_id)?,
                &encode_value(value)?,
            )?;
        }
    }
    batch.commit()
}

pub(super) fn apply_write(
    store: &dyn KeyValueStore,
    table: &str,
    doc_id: DocId,
    values: Option<&BTreeMap<String, Value>>,
) -> StorageBackendResult<()> {
    let fields = fields(store, table)?;
    let mut batch = store.batch();
    for field in fields {
        let key = btree_entry_key(table, &field, doc_id)?;
        if let Some(value) = values.and_then(|values| values.get(&field)) {
            batch.put(&key, &encode_value(value)?)?;
        } else {
            batch.delete(&key)?;
        }
    }
    batch.commit()
}

pub(super) fn drop_index(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<()> {
    let mut batch = store.batch();
    batch.delete(&btree_index_key(table, field)?)?;
    batch.delete_prefix(&btree_entry_field_prefix(table, field)?)?;
    batch.commit()
}

pub(super) fn clear_entries(store: &dyn KeyValueStore, table: &str) -> StorageBackendResult<()> {
    let fields = fields(store, table)?;
    let mut batch = store.batch();
    for field in fields {
        batch.delete_prefix(&btree_entry_field_prefix(table, &field)?)?;
    }
    batch.commit()
}
