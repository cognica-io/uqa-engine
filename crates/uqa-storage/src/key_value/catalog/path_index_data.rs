//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable path-index pairs and graph-scoped invalidation keys.

use super::{
    push_str, push_u64, read_str, read_u64, single_str_key, KeyValueBatch, KeyValueCatalog,
    StorageBackendError, StorageBackendResult, TAG_PATH_INDEX,
};
use crate::key_value::TAG_PATH_INDEX_DATA;
use crate::MAX_GRAPH_ID_PAGE;

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

impl KeyValueCatalog {
    pub(super) fn invalidate_graph_path_data(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
    ) -> StorageBackendResult<()> {
        let prefix = key(b'g', graph)?;
        let mut after = None;
        loop {
            let keys = self
                .store
                .scan_prefix_keys_after(&prefix, after.as_deref(), 256)?;
            if keys.is_empty() {
                break;
            }
            after = keys.last().cloned();
            for stored_key in keys {
                let mut offset = prefix.len();
                let index = read_str(&stored_key, &mut offset)?;
                batch.delete(&key(b'v', &index)?)?;
            }
        }
        Ok(())
    }
    pub(super) fn clear_path_index_data_into(
        &self,
        batch: &mut dyn KeyValueBatch,
        index: &str,
    ) -> StorageBackendResult<()> {
        let state = key(b's', index)?;
        if let Some(graph) = self.store.get(&state)? {
            let graph = String::from_utf8(graph)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            batch.delete(&reverse_key(&graph, index)?)?;
        }
        batch.delete_prefix(&key(b'p', index)?)?;
        batch.delete(&key(b'v', index)?)?;
        batch.delete(&state)
    }
    pub(super) fn clear_path_index_data_impl(&self, index: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.clear_path_index_data_into(batch.as_mut(), index)?;
        batch.commit()
    }
    pub(super) fn invalidate_path_index_data_into(
        batch: &mut dyn KeyValueBatch,
        index: &str,
    ) -> StorageBackendResult<()> {
        batch.delete(&key(b'v', index)?)
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
        if self
            .store
            .get(&single_str_key(TAG_PATH_INDEX, index)?)?
            .as_deref()
            != Some(definition.as_bytes())
        {
            return Err(StorageBackendError::Other(format!(
                "path index {index:?} definition changed during build"
            )));
        }
        let mut batch = self.store.batch();
        batch.put(&key(b's', index)?, graph.as_bytes())?;
        batch.put(&key(b'v', index)?, definition.as_bytes())?;
        batch.put(&reverse_key(graph, index)?, &[])?;
        batch.commit()
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
