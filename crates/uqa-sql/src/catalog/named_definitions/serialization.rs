//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native map and durable ordered-pair encodings preserve their existing catalog representations.

use super::{CapturedDefinitions, DefinitionKey, DefinitionRegistry, StoredDefinition};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

impl<K: Ord + Serialize, T: Serialize> Serialize for DefinitionRegistry<K, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (name, value) in self {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de, K: DefinitionKey + Deserialize<'de>, T: StoredDefinition<K::View> + Deserialize<'de>>
    Deserialize<'de> for DefinitionRegistry<K, T>
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = std::collections::BTreeMap::<K, T>::deserialize(deserializer)?;
        Self::try_from_iter(values).map_err(serde::de::Error::custom)
    }
}

impl<K: Ord + std::fmt::Debug, T: std::fmt::Debug> std::fmt::Debug for DefinitionRegistry<K, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self).finish()
    }
}

impl<K: Ord + Serialize, T: Serialize> Serialize for CapturedDefinitions<K, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut values = serializer.serialize_seq(Some(self.len()))?;
        for entry in self.iter() {
            values.serialize_element(&entry)?;
        }
        values.end()
    }
}

impl<K: Ord + std::fmt::Debug, T: std::fmt::Debug> std::fmt::Debug for CapturedDefinitions<K, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}
