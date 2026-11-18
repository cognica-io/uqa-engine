//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema lookup retirement preserves the original collection and per-entry owners.

use super::fields::{Entry, Sequence};
use crate::schema::{lookup_map::retirement::RetainedIndexSlots, SchemaMap};

pub(in crate::schema) enum Slots<K, V> {
    Ordinary(std::collections::hash_map::IntoIter<K, V>),
    Admitted(RetainedIndexSlots<K, V>),
}

impl<K, V> From<SchemaMap<K, V>> for Slots<K, V> {
    fn from(map: SchemaMap<K, V>) -> Self {
        match map {
            SchemaMap::Ordinary(map) => Self::Ordinary(map.into_iter()),
            SchemaMap::Admitted(map) => Self::Admitted(map.into()),
        }
    }
}

impl<K, V> Iterator for Slots<K, V> {
    type Item = Option<Entry<K, V>>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Ordinary(map) => map.next().map(|(key, value)| {
                Some(Entry {
                    key,
                    value,
                    memory: (None, None),
                })
            }),
            Self::Admitted(map) => map.next().map(|entry| {
                entry.map(|(key, value)| {
                    let (key, key_memory) = key.into_parts();
                    let (value, value_memory) = value.into_parts();
                    Entry {
                        key,
                        value,
                        memory: (key_memory, value_memory),
                    }
                })
            }),
        }
    }
}

pub(in crate::schema) type Map<K, V> = Sequence<Slots<K, V>>;
