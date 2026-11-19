//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable path-index pairs and graph-scoped invalidation keys.

use super::graph_view::GraphRead;
use super::{
    push_str, push_u64, read_str, read_u64, single_str_key, KeyValueBatch, KeyValueCatalog,
    StorageBackendError, StorageBackendResult, TAG_NAMED_GRAPH, TAG_PATH_INDEX,
};
use crate::key_value::codec::{
    key_segment_length, read_segment_length, read_str_ref, single_str_key_prefix,
};
use crate::key_value::view::for_each_key;
use crate::key_value::KeyValueRead;
use crate::key_value::TAG_PATH_INDEX_DATA;
use crate::MAX_GRAPH_ID_PAGE;

pub(super) const OWNER_STATE: u8 = b's';

fn key(kind: u8, name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vec![TAG_PATH_INDEX_DATA, kind];
    push_str(&mut key, name)?;
    Ok(key)
}
fn reverse_key(graph: &str, index: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key(b'g', graph)?;
    push_str(&mut key, index)?;
    Ok(key)
}
fn write_owner(
    batch: &mut dyn KeyValueBatch,
    index: &str,
    graph: &str,
) -> StorageBackendResult<()> {
    batch.put(&key(OWNER_STATE, index)?, graph.as_bytes())?;
    batch.put(&reverse_key(graph, index)?, &[])
}
fn pair_prefix(index: &str, sequence: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key(b'p', index)?;
    push_str(&mut key, sequence)?;
    Ok(key)
}
fn pair_key(mut prefix: Vec<u8>, source: u64, target: u64) -> Vec<u8> {
    push_u64(&mut prefix, source);
    push_u64(&mut prefix, target);
    prefix
}

impl GraphRead<'_> {
    pub(super) fn retain_path_owners(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        let Some(mut minimum) = key_segment_length(graph.len())?.checked_add(2) else {
            return Ok(());
        };
        let control = self.read.control();
        // Names sort by encoded byte length first. Seek each occupied length once,
        // then scan just the raw graph prefix within that length.
        let mut lower = [TAG_PATH_INDEX, 0, 0, 0, 0];
        loop {
            lower[1..].copy_from_slice(&minimum.to_be_bytes());
            let mut length = None;
            self.read.visit_keys_after(
                &[TAG_PATH_INDEX],
                Some(&lower),
                1,
                control,
                &mut |stored| {
                    length = Some(read_segment_length(stored, &mut 1)?);
                    Ok(())
                },
            )?;
            let Some(length) = length else { return Ok(()) };
            let mut prefix =
                single_str_key_prefix(TAG_PATH_INDEX, graph, length as usize, control.memory())?;
            prefix.extend_from_slice(b"::")?;
            for_each_key(self.read, &prefix, &mut |stored| {
                let index = read_str_ref(stored, &mut 1)?;
                if let Some(previous) = crate::catalog::path_index::owner_before_graph_creation(
                    index,
                    graph,
                    || self.contains(&key(OWNER_STATE, index)?),
                    |name| self.contains(&single_str_key(TAG_NAMED_GRAPH, name)?),
                )? {
                    if self.identifiers {
                        batch.require_unchanged(stored)?;
                        batch.require_unchanged(&single_str_key(TAG_NAMED_GRAPH, previous)?)?;
                    }
                    write_owner(batch, index, previous)?;
                }
                Ok(true)
            })?;
            let Some(next) = length.checked_add(1) else {
                return Ok(());
            };
            minimum = next;
        }
    }
}

impl KeyValueCatalog {
    pub(super) fn path_index_graph_from(
        read: &dyn KeyValueRead,
        index: &str,
    ) -> StorageBackendResult<Option<String>> {
        read.get(&key(OWNER_STATE, index)?)?
            .map(|value| {
                std::str::from_utf8(&value)
                    .map(str::to_owned)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))
            })
            .transpose()
    }

    pub(super) fn invalidate_graph_path_data(
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        batch.graph_mutation(crate::mvcc::GraphMutation::InvalidateGraph(graph))?;
        let prefix = key(b'g', graph)?;
        for_each_key(read, &prefix, &mut |stored_key| {
            let mut offset = prefix.len();
            let index = read_str(stored_key, &mut offset)?;
            if read.get(&key(OWNER_STATE, &index)?)?.as_deref() == Some(graph.as_bytes()) {
                batch.preview_graph_invalidation(&key(b'v', &index)?, None)?;
            }
            Ok(true)
        })
    }
    pub(super) fn clear_path_index_data_into(
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        index: &str,
    ) -> StorageBackendResult<()> {
        let state = key(OWNER_STATE, index)?;
        if let Some(graph) = read.get(&state)? {
            let graph = std::str::from_utf8(&graph)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            batch.delete(&reverse_key(graph, index)?)?;
        }
        batch.delete_prefix(&key(b'p', index)?)?;
        batch.replace_graph_cache(&key(b'v', index)?, None)?;
        batch.delete(&state)
    }
    pub(super) fn clear_path_index_data_impl(&self, index: &str) -> StorageBackendResult<()> {
        self.store
            .with_mutation(&mut |read, batch| Self::clear_path_index_data_into(read, batch, index))
    }
    pub(super) fn invalidate_path_index_data_into(
        batch: &mut dyn KeyValueBatch,
        index: &str,
    ) -> StorageBackendResult<()> {
        batch.replace_graph_cache(&key(b'v', index)?, None)
    }
    pub(super) fn save_path_index_pairs_impl(
        &self,
        index: &str,
        sequence: &str,
        pairs: &[(u64, u64)],
    ) -> StorageBackendResult<()> {
        if pairs.len() > MAX_GRAPH_ID_PAGE {
            return Err(StorageBackendError::Other(
                "path index batch exceeds the bounded page size".into(),
            ));
        }
        let prefix = pair_prefix(index, sequence)?;
        let mut batch = self.store.batch();
        for &(source, target) in pairs {
            batch.put(&pair_key(prefix.clone(), source, target), &[])?;
        }
        batch.commit()
    }
    pub(super) fn finish_path_index_data_impl(
        &self,
        index: &str,
        graph: &str,
        definition: &str,
    ) -> StorageBackendResult<()> {
        self.store.with_mutation(&mut |read, batch| {
            if read
                .get(&single_str_key(TAG_PATH_INDEX, index)?)?
                .as_deref()
                != Some(definition.as_bytes())
            {
                return Err(StorageBackendError::Other(format!(
                    "path index {index:?} definition changed during build"
                )));
            }
            if let Some(previous) = read.get(&key(OWNER_STATE, index)?)? {
                let previous = std::str::from_utf8(&previous)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                if previous != graph {
                    batch.delete(&reverse_key(previous, index)?)?;
                }
            }
            write_owner(batch, index, graph)?;
            batch.replace_graph_cache(&key(b'v', index)?, Some(definition.as_bytes()))?;
            batch.graph_mutation(crate::mvcc::GraphMutation::PublishPath {
                index,
                graph,
                definition,
            })?;
            Ok(())
        })
    }

    pub(super) fn path_index_data_is_current_impl(
        &self,
        index: &str,
        definition: &str,
    ) -> StorageBackendResult<bool> {
        Ok(self.store.get(&key(b'v', index)?)?.as_deref() == Some(definition.as_bytes()))
    }
    pub(super) fn path_index_pairs_impl(
        &self,
        index: &str,
        sequence: &str,
        after: Option<(u64, u64)>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(u64, u64)>> {
        crate::catalog::validate_graph_page(limit)?;
        let prefix = pair_prefix(index, sequence)?;
        let after = after.map(|(source, target)| pair_key(prefix.clone(), source, target));
        self.store
            .scan_prefix_keys_after(&prefix, after.as_deref(), limit)?
            .into_iter()
            .map(|key| {
                let mut offset = prefix.len();
                let source = read_u64(&key, &mut offset)?;
                let target = read_u64(&key, &mut offset)?;
                if offset != key.len() {
                    return Err(StorageBackendError::Other(
                        "malformed durable path-index pair key".into(),
                    ));
                }
                Ok((source, target))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogFacade, GraphSnapshot, KeyValueStore, MemoryKeyValueStore};
    use std::sync::Arc;

    #[test]
    fn new_graph_preserves_historical_owners_and_raw_cache_state_in_the_same_batch() {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        let old = "a";
        let new = "a::b";
        let paths = ["a::b::", "a::b::i", "a::b::longer", "a::b::한국"];
        catalog.save_named_graph(old).unwrap();
        for path in paths {
            catalog.save_path_index(path, "[]").unwrap();
            catalog
                .save_path_index_pairs(path, "edge", &[(3, 4)])
                .unwrap();
        }
        catalog.save_path_index("a::b2::i", "[]").unwrap();
        let built = "a::b::built";
        catalog.save_path_index(built, "[]").unwrap();
        catalog.finish_path_index_data(built, old, "[]").unwrap();
        store.begin_transaction().unwrap();
        catalog.save_named_graph(new).unwrap();
        for path in paths {
            assert_eq!(
                catalog.path_index_graph(path).unwrap().as_deref(),
                Some(old)
            );
            assert!(store
                .contains_key(&reverse_key(old, path).unwrap())
                .unwrap());
        }
        store.rollback_transaction().unwrap();
        assert!(!catalog.named_graph_exists(new).unwrap());
        for path in paths {
            assert_eq!(catalog.path_index_graph(path).unwrap(), None);
            assert!(!store
                .contains_key(&reverse_key(old, path).unwrap())
                .unwrap());
        }
        let snapshot = GraphSnapshot {
            vertices: Vec::new(),
            edges: Vec::new(),
            label_registry_json: "{}".into(),
        };
        catalog.replace_named_graph(new, &snapshot).unwrap();
        for path in paths {
            assert_eq!(
                catalog.path_index_graph(path).unwrap().as_deref(),
                Some(old)
            );
            assert!(store
                .contains_key(&reverse_key(old, path).unwrap())
                .unwrap());
            assert!(!catalog.path_index_data_is_current(path, "[]").unwrap());
            assert_eq!(
                catalog.path_index_pairs(path, "edge", None, 8).unwrap(),
                [(3, 4)]
            );
        }
        assert_eq!(catalog.path_index_graph("a::b2::i").unwrap(), None);
        assert_eq!(
            catalog.path_index_graph(built).unwrap().as_deref(),
            Some(old)
        );
        assert!(catalog.path_index_data_is_current(built, "[]").unwrap());
    }
}
