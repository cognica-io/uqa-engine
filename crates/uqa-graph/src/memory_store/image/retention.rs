//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph image admission follows the existing value payload model for standard maps and sets.

use super::{invalid, StorageBackendResult, StorageReadControl};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::memory::{BudgetedString, MemoryError, MemoryReservation};

pub(super) fn entries<T>(memory: &mut MemoryReservation, count: usize) -> StorageBackendResult<()> {
    memory.grow(
        count
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    Ok(())
}

pub(super) fn ordered<K: Ord, V>(map: &BTreeMap<K, V>, key: &K) -> StorageBackendResult<()> {
    if map
        .last_key_value()
        .is_some_and(|(previous, _)| previous >= key)
    {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub(super) fn copy(
    name: &str,
    memory: &mut MemoryReservation,
    control: &StorageReadControl,
) -> StorageBackendResult<String> {
    let mut text = BudgetedString::new(control.memory());
    text.reserve(name.len())?;
    for (offset, ch) in name.char_indices() {
        if offset % 1024 < ch.len_utf8() {
            control.check()?;
        }
        text.push(ch)?;
    }
    let (text, lease) = text.into_parts();
    memory.absorb(lease);
    Ok(text)
}

pub(super) fn label_index(
    map: &mut BTreeMap<String, BTreeSet<u64>>,
    label: &str,
    id: u64,
    memory: &mut MemoryReservation,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    if !map.contains_key(label) {
        entries::<(String, BTreeSet<u64>)>(memory, 1)?;
        let name = copy(label, memory, control)?;
        map.insert(name, BTreeSet::new());
    }
    entries::<u64>(memory, 1)?;
    map.get_mut(label).expect("admitted label index").insert(id);
    Ok(())
}

pub(super) fn adjacency(
    map: &mut BTreeMap<u64, BTreeSet<u64>>,
    vertex: u64,
    edge: u64,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<()> {
    if !map.contains_key(&vertex) {
        entries::<(u64, BTreeSet<u64>)>(memory, 1)?;
    }
    entries::<u64>(memory, 1)?;
    map.entry(vertex).or_default().insert(edge);
    Ok(())
}

pub(super) fn membership(
    map: &mut BTreeMap<u64, BTreeSet<String>>,
    id: u64,
    graph: &str,
    memory: &mut MemoryReservation,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    if !map.contains_key(&id) {
        entries::<(u64, BTreeSet<String>)>(memory, 1)?;
    }
    entries::<String>(memory, 1)?;
    let graph = copy(graph, memory, control)?;
    map.entry(id).or_default().insert(graph);
    Ok(())
}
