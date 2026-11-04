//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CapturedEventCatalog, EventCatalog, EventDefinition, EventDefinitions};
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Deserialize, Deserializer, Serialize, Serializer,
};

impl<T: Serialize> Serialize for EventDefinitions<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = serializer.serialize_map(Some(self.len()))?;
        for (name, value) in self {
            entries.serialize_entry(name, value)?;
        }
        entries.end()
    }
}
impl<'de, T: EventDefinition + Deserialize<'de>> Deserialize<'de> for EventDefinitions<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = std::collections::BTreeMap::<String, T>::deserialize(deserializer)?;
        Self::try_from_iter(entries).map_err(serde::de::Error::custom)
    }
}
impl<T: Serialize> Serialize for EventCatalog<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.relations.serialize(serializer)
    }
}
impl<'de, T: EventDefinition + Deserialize<'de>> Deserialize<'de> for EventCatalog<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            relations: Deserialize::deserialize(deserializer)?,
        })
    }
}
impl<T: Serialize> Serialize for CapturedEventCatalog<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut relations = serializer.serialize_seq(Some(self.len()))?;
        for (relation, entries) in self.iter() {
            relations.serialize_element(&(relation, CapturedEntries(entries)))?;
        }
        relations.end()
    }
}
struct CapturedEntries<'a, T>(&'a EventDefinitions<T>);
impl<T: Serialize> Serialize for CapturedEntries<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            entries.serialize_element(&entry)?;
        }
        entries.end()
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for EventDefinitions<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self).finish()
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for EventCatalog<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self).finish()
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for CapturedEventCatalog<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}
