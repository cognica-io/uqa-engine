//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded least-recently-used ownership; eviction cannot invalidate a caller's immutable handle.

use std::{collections::VecDeque, sync::Arc};

pub(super) struct Cache<K, V> {
    entries: VecDeque<(K, Arc<V>, usize)>,
    weight: usize,
}

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            weight: 0,
        }
    }
}

impl<K: PartialEq, V> Cache<K, V> {
    pub fn get(&mut self, key: &K) -> Option<Arc<V>> {
        let index = self
            .entries
            .iter()
            .position(|(stored, _, _)| stored == key)?;
        let entry = self.entries.remove(index).expect("located cache entry");
        let value = entry.1.clone();
        self.entries.push_front(entry);
        Some(value)
    }

    pub fn insert(
        &mut self,
        key: K,
        value: Arc<V>,
        weight: usize,
        maximum: usize,
        maximum_weight: usize,
    ) {
        if maximum == 0 || weight > maximum_weight {
            return;
        }
        while self.entries.len() >= maximum || weight > maximum_weight - self.weight {
            let (_, _, removed) = self.entries.pop_back().expect("occupied bounded cache");
            self.weight -= removed;
        }
        self.entries.push_front((key, value, weight));
        self.weight += weight;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn weight(&self) -> usize {
        self.weight
    }
}
