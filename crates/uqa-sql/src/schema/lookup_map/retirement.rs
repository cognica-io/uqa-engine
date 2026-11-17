//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted schema indexes retire their original storage through the same owning slot cursor.

use super::{Admitted, RetainedIndex};
use std::sync::Arc;
use uqa_core::memory::{BudgetedHashIndexSlots, MemoryReservation, Produced};

/// The last shared owner transfers both storage and the enclosing header/buffer lease. Other readers retain the original generation without copying or exposing its entries to retirement.
pub(in crate::schema) struct RetainedIndexSlots<K, V> {
    slots: Slots<K, V>,
    _memory: Option<MemoryReservation>,
}

enum Slots<K, V> {
    Hash(BudgetedHashIndexSlots<K, V>),
    Sorted(std::vec::IntoIter<(Produced<K>, Produced<V>)>),
}

impl<K, V> RetainedIndex<K, V> {
    fn take_slots(&mut self) -> Slots<K, V> {
        match self {
            Self::Hash(index) => Slots::Hash(index.drain_slots()),
            Self::Sorted(entries) => Slots::Sorted(std::mem::take(entries).into_iter()),
        }
    }
}

impl<K, V> From<Produced<RetainedIndex<K, V>>> for RetainedIndexSlots<K, V> {
    fn from(index: Produced<RetainedIndex<K, V>>) -> Self {
        let (mut index, memory) = index.into_parts();
        Self {
            slots: index.take_slots(),
            _memory: memory,
        }
    }
}

impl<K, V> From<Admitted<K, V>> for RetainedIndexSlots<K, V> {
    fn from(index: Admitted<K, V>) -> Self {
        match Arc::into_inner(index) {
            Some(index) => index.into(),
            None => Self {
                slots: Slots::Sorted(Vec::new().into_iter()),
                _memory: None,
            },
        }
    }
}

impl<K, V> Iterator for Slots<K, V> {
    type Item = Option<(Produced<K>, Produced<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Hash(slots) => slots
                .next()
                .map(|entry| entry.map(|(key, value)| (key.into(), value.into()))),
            Self::Sorted(entries) => entries.next().map(Some),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Hash(slots) => slots.size_hint(),
            Self::Sorted(entries) => entries.size_hint(),
        }
    }
}

impl<K, V> Iterator for RetainedIndexSlots<K, V> {
    type Item = Option<(Produced<K>, Produced<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.slots.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.slots.size_hint()
    }
}

impl<K, V> ExactSizeIterator for RetainedIndexSlots<K, V> {}
impl<K, V> std::iter::FusedIterator for RetainedIndexSlots<K, V> {}

impl<K, V> Drop for RetainedIndex<K, V> {
    fn drop(&mut self) {
        // The enclosing Produced object still holds its lease during this field's destructor.
        for entry in (RetainedIndexSlots {
            slots: self.take_slots(),
            _memory: None,
        }) {
            drop(entry);
        }
    }
}

#[cfg(test)]
mod tests;
