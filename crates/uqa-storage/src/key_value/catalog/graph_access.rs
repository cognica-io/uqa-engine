//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded graph cursors over durable label, adjacency, and membership keys.

use super::{
    decode_value, edge_key, graph_membership_graph_prefix, graph_membership_key, key_with_tag,
    push_str, push_u64, read_str, read_u64, single_str_key, vertex_key, EdgeRow, KeyValueBatch,
    KeyValueCatalog, StorageBackendError, StorageBackendResult, StoredEdge, StoredVertex, TAG_EDGE,
    TAG_METADATA, TAG_VERTEX,
};
use crate::key_value::TAG_GRAPH_LOOKUP;
use crate::{GraphEntityFilter, GraphEntityKind, GraphVertexRow};

const INDEX_VERSION: &str = "graph_lookup_indexes_v1";
static MIGRATION_SAVEPOINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn label_prefix(kind: GraphEntityKind, label: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vec![TAG_GRAPH_LOOKUP, b'l'];
    push_str(&mut key, kind.as_str())?;
    push_str(&mut key, label)?;
    Ok(key)
}

fn endpoint_prefix(outgoing: bool, vertex: u64) -> Vec<u8> {
    let mut key = vec![TAG_GRAPH_LOOKUP, if outgoing { b'o' } else { b'i' }];
    push_u64(&mut key, vertex);
    key
}

fn membership_prefix(kind: &str, id: u64) -> StorageBackendResult<Vec<u8>> {
    let mut key = vec![TAG_GRAPH_LOOKUP, b'm'];
    push_str(&mut key, kind)?;
    push_u64(&mut key, id);
    Ok(key)
}

pub(super) fn reverse_membership_key(
    kind: &str,
    id: u64,
    graph: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = membership_prefix(kind, id)?;
    push_str(&mut key, graph)?;
    Ok(key)
}

fn identity_key(mut prefix: Vec<u8>, id: u64) -> Vec<u8> {
    push_u64(&mut prefix, id);
    prefix
}

pub(super) fn vertex_lookup_keys(
    id: u64,
    row: &StoredVertex,
) -> StorageBackendResult<Vec<Vec<u8>>> {
    Ok(vec![identity_key(
        label_prefix(GraphEntityKind::Vertex, &row.label)?,
        id,
    )])
}

pub(super) fn edge_lookup_keys(id: u64, row: &StoredEdge) -> StorageBackendResult<Vec<Vec<u8>>> {
    Ok(vec![
        identity_key(label_prefix(GraphEntityKind::Edge, &row.label)?, id),
        identity_key(endpoint_prefix(true, row.source_id), id),
        identity_key(endpoint_prefix(false, row.target_id), id),
    ])
}

impl KeyValueCatalog {
    fn graph_lookup_indexes_ready(&self) -> StorageBackendResult<bool> {
        match self
            .store
            .get(&single_str_key(TAG_METADATA, INDEX_VERSION)?)?
        {
            Some(version) if version == b"1" => Ok(true),
            None => Ok(false),
            Some(_) => Err(StorageBackendError::Other(
                "unsupported graph lookup index version".into(),
            )),
        }
    }

    fn require_graph_lookup_indexes(&self) -> StorageBackendResult<()> {
        if self.graph_lookup_indexes_ready()? {
            return Ok(());
        }
        for prefix in [
            key_with_tag(TAG_VERTEX),
            key_with_tag(TAG_EDGE),
            super::graph_membership_prefix(),
        ] {
            if !self
                .store
                .scan_prefix_keys_after(&prefix, None, 1)?
                .is_empty()
            {
                return Err(StorageBackendError::Other(
                    "graph lookup indexes require an explicit catalog migration".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn delete_graph_memberships_into(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.invalidate_graph_path_data(batch, graph)?;
        let prefix = graph_membership_graph_prefix(graph)?;
        let mut after = None;
        loop {
            let keys = self
                .store
                .scan_prefix_keys_after(&prefix, after.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            after = keys.last().cloned();
            for key in keys {
                let mut offset = prefix.len();
                let kind = read_str(&key, &mut offset)?;
                let id = read_u64(&key, &mut offset)?;
                batch.delete(&reverse_membership_key(&kind, id, graph)?)?;
            }
        }
        batch.delete_prefix(&prefix)
    }

    pub(super) fn ensure_graph_lookup_indexes(&self) -> StorageBackendResult<()> {
        let _guard = self.graph_indexes_lock.lock();
        let marker = single_str_key(TAG_METADATA, INDEX_VERSION)?;
        if self.graph_lookup_indexes_ready()? {
            return Ok(());
        }
        let owned = !self.store.in_transaction();
        let savepoint = format!(
            "uqa_graph_index_migration_{}",
            MIGRATION_SAVEPOINT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        if owned {
            self.store.begin_transaction()?;
        } else {
            self.store.savepoint(&savepoint)?;
        }
        let result = (|| {
            if self.graph_lookup_indexes_ready()? {
                return Ok(());
            }
            self.store.delete_prefix(&[TAG_GRAPH_LOOKUP])?;
            for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                let prefix = key_with_tag(if kind == GraphEntityKind::Vertex {
                    TAG_VERTEX
                } else {
                    TAG_EDGE
                });
                let mut after = None;
                loop {
                    let rows = self
                        .store
                        .scan_prefix_after(&prefix, after.as_deref(), 256)?;
                    if rows.is_empty() {
                        break;
                    }
                    after = rows.last().map(|(key, _)| key.clone());
                    let mut batch = self.store.batch();
                    for (key, value) in rows {
                        let mut offset = 1;
                        let id = read_u64(&key, &mut offset)?;
                        let keys = match kind {
                            GraphEntityKind::Vertex => {
                                vertex_lookup_keys(id, &decode_value(&value)?)?
                            }
                            GraphEntityKind::Edge => edge_lookup_keys(id, &decode_value(&value)?)?,
                        };
                        for key in keys {
                            batch.put(&key, &[])?;
                        }
                    }
                    batch.commit()?;
                }
            }
            let prefix = super::graph_membership_prefix();
            let mut after = None;
            loop {
                let keys = self
                    .store
                    .scan_prefix_keys_after(&prefix, after.as_deref(), 256)?;
                if keys.is_empty() {
                    break;
                }
                after = keys.last().cloned();
                let mut batch = self.store.batch();
                for key in keys {
                    let mut offset = 1;
                    let graph = read_str(&key, &mut offset)?;
                    let kind = read_str(&key, &mut offset)?;
                    let id = read_u64(&key, &mut offset)?;
                    batch.put(&reverse_membership_key(&kind, id, &graph)?, &[])?;
                }
                batch.commit()?;
            }
            self.store.put(&marker, b"1")
        })();
        let result = result.and_then(|()| {
            if owned {
                self.store.commit_transaction()
            } else {
                self.store.release_savepoint(&savepoint)
            }
        });
        match result {
            Ok(()) => Ok(()),
            Err(error) => match if owned {
                self.store.rollback_transaction()
            } else {
                self.store
                    .rollback_to_savepoint(&savepoint)
                    .and_then(|()| self.store.release_savepoint(&savepoint))
            } {
                Ok(()) => Err(error),
                Err(rollback) => Err(StorageBackendError::Other(format!(
                    "graph index migration failed: {error}; rollback failed: {rollback}"
                ))),
            },
        }
    }

    pub(super) fn graph_vertex_impl(
        &self,
        id: u64,
    ) -> StorageBackendResult<Option<GraphVertexRow>> {
        self.store
            .get(&vertex_key(id))?
            .map(|value| {
                let row: StoredVertex = decode_value(&value)?;
                Ok(GraphVertexRow {
                    vertex_id: id,
                    label: row.label,
                    properties_json: row.properties_json,
                })
            })
            .transpose()
    }

    pub(super) fn graph_edge_impl(&self, id: u64) -> StorageBackendResult<Option<EdgeRow>> {
        self.store
            .get(&edge_key(id))?
            .map(|value| {
                let row: StoredEdge = decode_value(&value)?;
                Ok(EdgeRow {
                    edge_id: id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties_json: row.properties_json,
                })
            })
            .transpose()
    }

    pub(super) fn graph_entity_ids_impl(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> StorageBackendResult<Vec<u64>> {
        filter.validate()?;
        crate::catalog::validate_graph_page(limit)?;
        if filter.source.is_some() || filter.target.is_some() || filter.label.is_some() {
            self.require_graph_lookup_indexes()?;
        }
        let prefix = if let Some(source) = filter.source {
            endpoint_prefix(true, source)
        } else if let Some(target) = filter.target {
            endpoint_prefix(false, target)
        } else if let Some(label) = filter.label {
            label_prefix(filter.kind, label)?
        } else if let Some(graph) = filter.graph {
            let mut key = graph_membership_graph_prefix(graph)?;
            push_str(&mut key, filter.kind.as_str())?;
            key
        } else {
            key_with_tag(if filter.kind == GraphEntityKind::Vertex {
                TAG_VERTEX
            } else {
                TAG_EDGE
            })
        };
        let mut cursor = after.map(|id| identity_key(prefix.clone(), id));
        let mut result = Vec::new();
        loop {
            let keys = self
                .store
                .scan_prefix_keys_after(&prefix, cursor.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            cursor = keys.last().cloned();
            for key in keys {
                let mut offset = prefix.len();
                let id = read_u64(&key, &mut offset)?;
                if let Some(graph) = filter.graph {
                    if !self.store.contains_key(&graph_membership_key(
                        filter.kind.as_str(),
                        id,
                        graph,
                    )?)? {
                        continue;
                    }
                }
                let matches = match filter.kind {
                    GraphEntityKind::Vertex => {
                        if filter.label.is_none() {
                            self.store.contains_key(&vertex_key(id))?
                        } else {
                            self.graph_vertex_impl(id)?.is_some_and(|row| {
                                filter.label.is_none_or(|label| row.label == label)
                            })
                        }
                    }
                    GraphEntityKind::Edge => {
                        if filter.label.is_none()
                            && filter.source.is_none()
                            && filter.target.is_none()
                        {
                            self.store.contains_key(&edge_key(id))?
                        } else {
                            self.graph_edge_impl(id)?.is_some_and(|row| {
                                filter.label.is_none_or(|label| row.label == label)
                                    && filter.source.is_none_or(|source| row.source_id == source)
                                    && filter.target.is_none_or(|target| row.target_id == target)
                            })
                        }
                    }
                };
                if !matches {
                    // A surviving index/membership must never conceal an absent entity.
                    let exists =
                        self.store
                            .contains_key(&if filter.kind == GraphEntityKind::Vertex {
                                vertex_key(id)
                            } else {
                                edge_key(id)
                            })?;
                    if !exists {
                        return Err(StorageBackendError::Other(format!(
                            "graph lookup references missing {} {id}",
                            filter.kind.as_str()
                        )));
                    }
                    continue;
                }
                result.push(id);
                if result.len() == limit {
                    return Ok(result);
                }
            }
        }
        Ok(result)
    }

    pub(super) fn graph_entity_memberships_impl(
        &self,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<Vec<String>> {
        self.require_graph_lookup_indexes()?;
        let prefix = membership_prefix(kind.as_str(), id)?;
        let mut after = None;
        let mut graphs = Vec::new();
        loop {
            let keys = self
                .store
                .scan_prefix_keys_after(&prefix, after.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            after = keys.last().cloned();
            for key in keys {
                let mut offset = prefix.len();
                graphs.push(read_str(&key, &mut offset)?);
            }
        }
        graphs.sort();
        Ok(graphs)
    }

    pub(super) fn replace_vertex_lookup(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        row: Option<&StoredVertex>,
    ) -> StorageBackendResult<()> {
        for graph in self.graph_entity_memberships_impl(GraphEntityKind::Vertex, id)? {
            self.invalidate_graph_path_data(batch, &graph)?;
        }
        if let Some(old) = self.store.get(&vertex_key(id))? {
            for key in vertex_lookup_keys(id, &decode_value(&old)?)? {
                batch.delete(&key)?;
            }
        }
        if let Some(row) = row {
            for key in vertex_lookup_keys(id, row)? {
                batch.put(&key, &[])?;
            }
        }
        Ok(())
    }

    pub(super) fn replace_edge_lookup(
        &self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
        row: Option<&StoredEdge>,
    ) -> StorageBackendResult<()> {
        for graph in self.graph_entity_memberships_impl(GraphEntityKind::Edge, id)? {
            self.invalidate_graph_path_data(batch, &graph)?;
        }
        if let Some(old) = self.store.get(&edge_key(id))? {
            for key in edge_lookup_keys(id, &decode_value(&old)?)? {
                batch.delete(&key)?;
            }
        }
        if let Some(row) = row {
            for key in edge_lookup_keys(id, row)? {
                batch.put(&key, &[])?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyValueStore, MemoryKeyValueStore};
    use std::sync::Arc;

    #[test]
    fn legacy_index_reads_fail_explicitly_without_migrating_or_hiding_rows() {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, None);
        filter.label = Some("Item");
        assert!(catalog
            .graph_entity_ids_impl(filter, None, 1)
            .unwrap()
            .is_empty());
        let row = StoredVertex {
            label: "Item".into(),
            properties_json: "{}".into(),
        };
        store
            .put(&vertex_key(1), &super::super::encode_value(&row).unwrap())
            .unwrap();
        store.begin_read_transaction().unwrap();
        assert!(catalog
            .graph_entity_ids_impl(filter, None, 1)
            .unwrap_err()
            .to_string()
            .contains("explicit catalog migration"));
        assert!(!store.transaction_has_written().unwrap());
        store.rollback_transaction().unwrap();
        catalog.ensure_graph_lookup_indexes().unwrap();
        assert_eq!(
            catalog.graph_entity_ids_impl(filter, None, 1).unwrap(),
            vec![1]
        );
        store
            .put(
                &single_str_key(TAG_METADATA, INDEX_VERSION).unwrap(),
                b"invalid-version",
            )
            .unwrap();
        assert!(catalog.graph_entity_ids_impl(filter, None, 1).is_err());
        assert!(catalog.ensure_graph_lookup_indexes().is_err());
    }

    #[test]
    fn failed_index_migration_rolls_back_its_pages_inside_an_outer_transaction() {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        let row = StoredVertex {
            label: "Item".into(),
            properties_json: "{}".into(),
        };
        let encoded = super::super::encode_value(&row).unwrap();
        for id in 1..=300 {
            store.put(&vertex_key(id), &encoded).unwrap();
        }
        store.put(&vertex_key(301), b"invalid-record").unwrap();
        let preserved = [TAG_GRAPH_LOOKUP, b'!'];
        store.put(&preserved, b"previous-index-state").unwrap();
        store.begin_transaction().unwrap();
        store.put(b"unrelated-outer-write", b"kept").unwrap();
        assert!(catalog.ensure_graph_lookup_indexes().is_err());
        assert!(store.in_transaction());
        assert_eq!(
            store.get(&preserved).unwrap(),
            Some(b"previous-index-state".to_vec())
        );
        assert!(!store
            .contains_key(&identity_key(
                label_prefix(GraphEntityKind::Vertex, "Item").unwrap(),
                1
            ))
            .unwrap());
        assert!(!catalog.graph_lookup_indexes_ready().unwrap());
        store.commit_transaction().unwrap();
        assert_eq!(
            store.get(b"unrelated-outer-write").unwrap(),
            Some(b"kept".to_vec())
        );
        store.put(&vertex_key(301), &encoded).unwrap();
        catalog.ensure_graph_lookup_indexes().unwrap();
        assert!(catalog.graph_lookup_indexes_ready().unwrap());
    }
}
