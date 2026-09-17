//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded graph reconstruction and exact agreement with the canonical tensors.

use crate::mvcc::{
    CommittedRecordSnapshot, HNSWRecordHeader, HNSWRecordKey as Key, HNSWRecordLayout,
    VersionError, VersionResult,
};
use crate::{
    hnsw_index::{HNSWIndex, HNSWNodeSnapshot},
    read_control::StorageReadControl,
};
use std::collections::BTreeMap;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError, MemoryReservation};

pub(super) fn index(
    key: &[u8],
    header: HNSWRecordHeader,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn HNSWRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<Budgeted<HNSWIndex>> {
    let invalid = || VersionError::InvalidEncoding("HNSW graph disagrees with canonical tensors");
    let mut nodes = BudgetedVec::<HNSWNodeSnapshot>::new(control.memory());
    let mut payload = control.memory().reserve(0)?;
    let mut lookup_memory = control.memory().reserve(0)?;
    let mut positions = BTreeMap::new();
    current.visit_prefix(
        &layout.key(key, Key::Nodes, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let node = layout.node(key, value, control)?;
                nodes.reserve(1)?;
                lookup_memory.grow(size_of::<(u64, usize)>())?;
                if positions.insert(node.node_id, nodes.len()).is_some() {
                    return Err(invalid());
                }
                let (node, memory) = node.into_parts();
                payload.absorb(memory);
                nodes.push(node)?;
            }
            Ok(true)
        },
    )?;
    if let Some(edges) = layout.edges_prefix(key, None, control)? {
        current.visit_prefix(&edges, None, usize::MAX, control, &mut |key, row| {
            if let Some(value) = row.value {
                let (source, layer, target) = layout.edge(key, value, control)?;
                let position = *positions.get(&source).ok_or_else(invalid)?;
                let neighbors = nodes[position]
                    .neighbors
                    .get_mut(layer)
                    .ok_or_else(invalid)?;
                append_neighbor(neighbors, &mut payload, target)?;
            }
            Ok(true)
        })?;
    }
    drop(positions);
    drop(lookup_memory);
    let lookup_bytes = nodes
        .len()
        .checked_mul(size_of::<((u64, u32), &[f32])>())
        .ok_or(MemoryError::SizeOverflow)?;
    let canonical_memory = control.memory().reserve(lookup_bytes)?;
    let mut live = BTreeMap::new();
    for node in nodes.iter().filter(|node| !node.deleted) {
        control.cancellation().check()?;
        if live
            .insert((node.doc_id, node.vector_ordinal), &*node.raw_vector)
            .is_some()
        {
            return Err(invalid());
        }
    }
    current.visit_prefix(
        &layout.key(key, Key::Vectors, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (document, ordinal, vector) = layout.vector(key, value, control)?;
                let expected = live.remove(&(document, ordinal)).ok_or_else(invalid)?;
                if vector.len() != expected.len() {
                    return Err(invalid());
                }
                for (actual, expected) in vector.iter().zip(expected) {
                    control.cancellation().check()?;
                    if actual.to_bits() != expected.to_bits() {
                        return Err(invalid());
                    }
                }
            }
            Ok(true)
        },
    )?;
    if !live.is_empty() {
        return Err(invalid());
    }
    drop(live);
    drop(canonical_memory);
    let (nodes, node_memory) = nodes.into_parts();
    let index = HNSWIndex::from_persistence_controlled(
        header.dimensions,
        header.params,
        header.meta,
        nodes,
        control,
    )?;
    drop((payload, node_memory));
    Ok(index)
}

fn append_neighbor(
    neighbors: &mut Vec<u64>,
    payload: &mut MemoryReservation,
    target: u64,
) -> VersionResult<()> {
    if neighbors.len() == neighbors.capacity() {
        let old_bytes = neighbors
            .capacity()
            .checked_mul(size_of::<u64>())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut replacement = BudgetedVec::new(payload.budget());
        replacement.reserve(
            neighbors
                .len()
                .checked_add(1)
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        replacement.extend_from_slice(neighbors)?;
        let (values, memory) = replacement.into_parts();
        // Keep both allocations charged until the original adjacency buffer is freed.
        drop(std::mem::replace(neighbors, values));
        drop(payload.split(old_bytes));
        payload.absorb(memory);
    }
    neighbors.push(target);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_edge_buffers_charge_spare_capacity_and_preserve_failed_growth() {
        let control = StorageReadControl::with_limit(1024);
        let mut payload = control.memory().reserve(0).unwrap();
        let mut neighbors = Vec::new();
        for target in 0..4 {
            append_neighbor(&mut neighbors, &mut payload, target).unwrap();
        }
        while neighbors.len() < neighbors.capacity() {
            append_neighbor(&mut neighbors, &mut payload, 99).unwrap();
        }
        assert_eq!(
            control.memory().used(),
            neighbors.capacity() * size_of::<u64>()
        );
        let before = neighbors.clone();
        let hold = control
            .memory()
            .reserve(1024 - control.memory().used())
            .unwrap();
        assert!(matches!(
            append_neighbor(&mut neighbors, &mut payload, 100),
            Err(VersionError::Memory(_))
        ));
        assert_eq!(neighbors, before);
        drop(hold);
        append_neighbor(&mut neighbors, &mut payload, 100).unwrap();
        assert_eq!(
            control.memory().used(),
            neighbors.capacity() * size_of::<u64>()
        );
        drop((neighbors, payload));
        assert_eq!(control.memory().used(), 0);
    }
}
