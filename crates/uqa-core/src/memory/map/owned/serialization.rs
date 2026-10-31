//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the ordered-map wire representation without an intermediate map allocation.

use super::OwnedMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

impl<K: Serialize, V: Serialize> Serialize for OwnedMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Deserialize<'de> for OwnedMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<K, V>(std::marker::PhantomData<(K, V)>);
        impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> serde::de::Visitor<'de>
            for Visitor<K, V>
        {
            type Value = OwnedMap<K, V>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an ordered map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut output = OwnedMap::new();
                while let Some((key, value)) = map.next_entry()? {
                    output.insert(key, value);
                }
                Ok(output)
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}
