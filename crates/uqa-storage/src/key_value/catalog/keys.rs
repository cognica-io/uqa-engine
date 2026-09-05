//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog key construction, batch re-keying, and prefix row decoding.

use super::{
    decode_string, key_with_tag, push_str, push_u64, read_str, CatalogIndexRow, KeyValueBatch,
    KeyValueStore, RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult,
    TAG_COLUMN_STATS, TAG_EDGE, TAG_GRAPH_MEMBERSHIP, TAG_TABLE_FIELD_ANALYZER, TAG_VERTEX,
};

pub(super) fn relation_key(tag: u8, relation: &RelationIdentity) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, &relation.schema)?;
    push_str(&mut key, &relation.name)?;
    Ok(key)
}

pub(super) fn decode_relation_key(key: &[u8]) -> StorageBackendResult<RelationIdentity> {
    let mut offset = 1;
    let schema = read_str(key, &mut offset)?;
    let name = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(StorageBackendError::Other(
            "corrupt relation catalog key has trailing bytes".into(),
        ));
    }
    Ok(RelationIdentity::new(schema, name))
}

pub(super) fn decode_catalog_relation_key(
    key: &[u8],
) -> StorageBackendResult<(RelationIdentity, bool, String)> {
    let mut offset = 1;
    let first = read_str(key, &mut offset)?;
    if offset == key.len() {
        let relation =
            RelationIdentity::from_legacy_name(&first).map_err(StorageBackendError::Other)?;
        return Ok((relation, true, first));
    }
    let second = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(StorageBackendError::Other(
            "corrupt relation catalog key has trailing bytes".into(),
        ));
    }
    let relation = RelationIdentity::new(first, second);
    Ok((relation.clone(), false, relation.qualified_name()))
}

pub(super) fn register_migration_relation(
    seen: &mut std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>,
    relation: &RelationIdentity,
    kind: RelationKind,
    source: String,
) -> StorageBackendResult<()> {
    if let Some((existing_kind, existing_source)) = seen.get(relation) {
        return Err(StorageBackendError::Other(format!(
            "relation namespace migration collision for `{}`: {} `{}` and {} `{}`",
            relation.qualified_name(),
            existing_kind.as_str(),
            existing_source,
            kind.as_str(),
            source
        )));
    }
    seen.insert(relation.clone(), (kind, source));
    Ok(())
}

pub(super) fn ensure_prefix_absent(
    store: &dyn KeyValueStore,
    prefix: &[u8],
    relation: &RelationIdentity,
) -> StorageBackendResult<()> {
    if !store.scan_prefix(prefix)?.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "relation namespace migration for `{}` would overwrite existing table data",
            relation.qualified_name()
        )));
    }
    Ok(())
}

pub(super) fn graph_membership_prefix() -> Vec<u8> {
    key_with_tag(TAG_GRAPH_MEMBERSHIP)
}

pub(super) fn graph_membership_graph_prefix(graph_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = graph_membership_prefix();
    push_str(&mut key, graph_name)?;
    Ok(key)
}

pub(super) fn graph_membership_key(
    entity_type: &str,
    entity_id: u64,
    graph_name: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = graph_membership_graph_prefix(graph_name)?;
    push_str(&mut key, entity_type)?;
    push_u64(&mut key, entity_id);
    Ok(key)
}

pub(super) fn table_field_analyzer_prefix(table_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(TAG_TABLE_FIELD_ANALYZER);
    push_str(&mut key, table_name)?;
    Ok(key)
}

pub(super) fn table_field_analyzer_key(
    table_name: &str,
    field: &str,
    phase: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = table_field_analyzer_field_prefix(table_name, field)?;
    push_str(&mut key, phase)?;
    Ok(key)
}

pub(super) fn table_field_analyzer_field_prefix(
    table_name: &str,
    field: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = table_field_analyzer_prefix(table_name)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn column_stats_prefix(table_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(TAG_COLUMN_STATS);
    push_str(&mut key, table_name)?;
    Ok(key)
}

pub(super) fn column_stats_key(
    table_name: &str,
    column_name: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = column_stats_prefix(table_name)?;
    push_str(&mut key, column_name)?;
    Ok(key)
}

pub(super) fn vertex_key(vertex_id: u64) -> Vec<u8> {
    let mut key = key_with_tag(TAG_VERTEX);
    push_u64(&mut key, vertex_id);
    key
}

pub(super) fn edge_key(edge_id: u64) -> Vec<u8> {
    let mut key = key_with_tag(TAG_EDGE);
    push_u64(&mut key, edge_id);
    key
}

pub(super) fn batch_rekey_prefix(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        batch.put(&new_key, &value)?;
        batch.delete(&key)?;
    }
    Ok(())
}

pub(super) fn batch_rekey_prefix_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        if store.get(&new_key)?.is_none() {
            batch.put(&new_key, &value)?;
        }
        batch.delete(&key)?;
    }
    Ok(())
}

pub(super) fn batch_put_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    key: &[u8],
    value: &[u8],
) -> StorageBackendResult<()> {
    if store.get(key)?.is_none() {
        batch.put(key, value)?;
    }
    Ok(())
}

pub(super) fn catalog_index_references_column(
    row: &CatalogIndexRow,
    column_name: &str,
) -> StorageBackendResult<bool> {
    Ok(crate::catalog_index_keys::references_column(
        &row.columns_json,
        column_name,
    )?)
}

pub(super) fn catalog_index_rename_column(
    row: &CatalogIndexRow,
    from: &str,
    to: &str,
) -> StorageBackendResult<Option<String>> {
    Ok(crate::catalog_index_keys::rename_column(
        &row.columns_json,
        from,
        to,
    )?)
}

pub(super) fn load_single_keys(
    store: &dyn KeyValueStore,
    tag: u8,
) -> StorageBackendResult<Vec<String>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, _)| {
            let mut offset = 1;
            read_str(&key, &mut offset)
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

pub(super) fn load_single_string_rows(
    store: &dyn KeyValueStore,
    tag: u8,
) -> StorageBackendResult<Vec<(String, String)>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, value)| {
            let mut offset = 1;
            Ok((read_str(&key, &mut offset)?, decode_string(value)?))
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}
