//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded graph cursors over durable label, adjacency, and membership keys.

use super::{
    decode_value, key_with_tag, push_str, push_u64, read_str, read_u64, single_str_key, EdgeRow,
    KeyValueBatch, KeyValueCatalog, StorageBackendError, StorageBackendResult, StoredEdge,
    StoredVertex, TAG_EDGE, TAG_METADATA, TAG_VERTEX,
};
use crate::key_value::TAG_GRAPH_LOOKUP;
use crate::{GraphEntityFilter, GraphEntityKind, GraphVertexRow};

pub(super) const INDEX_VERSION: &str = "graph_lookup_indexes_v1";
static MIGRATION_SAVEPOINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) fn label_prefix(kind: GraphEntityKind, label: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vec![TAG_GRAPH_LOOKUP, b'l'];
    push_str(&mut key, kind.as_str())?;
    push_str(&mut key, label)?;
    Ok(key)
}

pub(super) fn endpoint_prefix(outgoing: bool, vertex: u64) -> Vec<u8> {
    let mut key = vec![TAG_GRAPH_LOOKUP, if outgoing { b'o' } else { b'i' }];
    push_u64(&mut key, vertex);
    key
}

pub(super) fn membership_prefix(kind: &str, id: u64) -> StorageBackendResult<Vec<u8>> {
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

pub(super) fn identity_key(mut prefix: Vec<u8>, id: u64) -> Vec<u8> {
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
    pub(super) fn with_graph_read<T>(
        &self,
        operation: impl FnOnce(super::graph_view::GraphRead<'_>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let identifiers = self.store.identifier_allocator().is_some();
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            operation(super::graph_view::GraphRead { read, identifiers })
        })
    }

    pub(super) fn with_graph_mutation(
        &self,
        operation: impl FnOnce(
            &super::graph_view::GraphRead<'_>,
            &mut dyn KeyValueBatch,
        ) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.ensure_graph_lookup_indexes()?;
        let identifiers = self.store.identifier_allocator().is_some();
        crate::key_value::index_view::evaluate_mutation(self.store.as_ref(), |read, batch| {
            operation(&super::graph_view::GraphRead { read, identifiers }, batch)
        })
    }

    fn graph_lookup_indexes_ready(&self) -> StorageBackendResult<bool> {
        crate::key_value::index_view::read_view(
            self.store.as_ref(),
            super::graph_view::graph_lookup_indexes_ready,
        )
    }

    pub(super) fn graph_vertex_impl(
        &self,
        id: u64,
    ) -> StorageBackendResult<Option<GraphVertexRow>> {
        self.with_graph_read(|read| read.vertex(id))
    }

    pub(super) fn graph_edge_impl(&self, id: u64) -> StorageBackendResult<Option<EdgeRow>> {
        self.with_graph_read(|read| read.edge(id))
    }

    pub(super) fn graph_entity_ids_impl(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> StorageBackendResult<Vec<u64>> {
        self.with_graph_read(|read| read.ids(filter, after, limit))
    }

    pub(super) fn graph_entity_memberships_impl(
        &self,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<Vec<String>> {
        self.with_graph_read(|read| read.memberships(kind, id))
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
}

#[cfg(test)]
mod tests {
    use super::super::vertex_key;
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
