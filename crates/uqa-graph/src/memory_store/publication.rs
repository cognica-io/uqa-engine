//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every mutation prepares complete private roots before replacing the live generation.

use super::{
    BTreeMap, GraphStoreResult, MemoryBudget, MemoryGraphStore, NativeLabels, NativeMap, NativeSet,
    Partition,
};
use uqa_core::{memory::MemoryError, Value};

impl Default for MemoryGraphStore {
    fn default() -> Self {
        Self::empty(&MemoryBudget::new(usize::MAX))
    }
}

impl MemoryGraphStore {
    fn empty(memory: &MemoryBudget) -> Self {
        Self {
            vertices: NativeMap::new(memory),
            edges: NativeMap::new(memory),
            graphs: NativeMap::new(memory),
            vertex_membership: NativeMap::new(memory),
            edge_membership: NativeMap::new(memory),
            label_registries: NativeMap::new(memory),
            next_vertex_id: 0,
            next_edge_id: 0,
            image_memory: None,
        }
    }

    /// Create primary graph storage whose native nodes and payloads retain this allowance through publication, transaction snapshots and source retirement.
    pub fn with_memory(memory: &MemoryBudget) -> Self {
        let mut store = Self::empty(memory);
        store.next_vertex_id = 1;
        store.next_edge_id = 1;
        store
    }

    pub(super) fn publish<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        let mut candidate = self.clone();
        let result = operation(&mut candidate)?;
        self.publish_generation(candidate);
        Ok(result)
    }

    pub(super) fn publish_generation(&mut self, mut candidate: Self) {
        // The outer allocation stays in place across publication and rollback; only a separately cloned owner receives a native header.
        candidate.image_memory = self.image_memory.take();
        drop(std::mem::replace(self, candidate));
    }

    pub(super) fn put_partition(
        &mut self,
        name: &str,
        partition: Partition,
    ) -> GraphStoreResult<()> {
        let retained = partition.retained_bytes()?;
        self.graphs.insert_named(name, partition, 0, retained)?;
        Ok(())
    }

    pub(super) fn put_registry(
        &mut self,
        graph: &str,
        registry: NativeLabels,
    ) -> GraphStoreResult<()> {
        let bytes = registry.retained_bytes()?;
        self.label_registries
            .insert_named(graph, registry, 0, bytes)?;
        Ok(())
    }
}

pub(super) fn membership(
    map: &mut NativeMap<u64, NativeSet<String>>,
    id: u64,
    graph: &str,
    add: bool,
) -> Result<(), MemoryError> {
    let existing = map.get(&id);
    if !add && existing.is_none() {
        return Ok(());
    }
    let mut names = existing
        .cloned()
        .unwrap_or_else(|| NativeSet::new(map.budget()))
        .into_mutation_budget(map.budget());
    if add {
        names.insert_name(graph)?;
    } else {
        names.remove(graph)?;
    }
    let bytes = names.retained_bytes()?;
    map.insert(id, names, 0, bytes)?;
    Ok(())
}

pub(super) fn entity_bytes(
    label: &String,
    properties: &BTreeMap<String, Value>,
    memory: &MemoryBudget,
) -> uqa_storage::StorageBackendResult<usize> {
    entity_bytes_with_check(label, properties, memory, || Ok(()))
}

pub(super) fn entity_bytes_with_check(
    label: &String,
    properties: &BTreeMap<String, Value>,
    memory: &MemoryBudget,
    mut check: impl FnMut() -> Result<(), uqa_core::QueryCancelled>,
) -> uqa_storage::StorageBackendResult<usize> {
    let mut bytes = label
        .capacity()
        .checked_add(
            properties
                .len()
                .checked_mul(size_of::<(String, Value)>())
                .ok_or(MemoryError::SizeOverflow)?,
        )
        .ok_or(MemoryError::SizeOverflow)?;
    for (name, value) in properties {
        check()?;
        let payload = value.retained_payload_bytes_with_check(memory, &mut check)?;
        bytes = bytes
            .checked_add(name.capacity())
            .and_then(|n| n.checked_add(payload))
            .ok_or(MemoryError::SizeOverflow)?;
    }
    Ok(bytes)
}

/// Merge an existing sorted owner set with one requested name without copying it.
pub(super) fn owners<'a>(
    existing: Option<&'a NativeSet<String>>,
    extra: &'a str,
) -> impl Iterator<Item = &'a str> {
    let mut names = existing
        .into_iter()
        .flat_map(|names| names.iter().map(String::as_str))
        .peekable();
    let mut extra = Some(extra);
    std::iter::from_fn(move || match (names.peek().copied(), extra) {
        (Some(name), Some(additional)) if additional < name => extra.take(),
        (Some(name), Some(additional)) if additional == name => {
            extra = None;
            names.next()
        }
        (Some(_), _) => names.next(),
        (None, _) => extra.take(),
    })
}
