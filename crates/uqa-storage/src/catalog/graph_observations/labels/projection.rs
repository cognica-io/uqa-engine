//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode only structural registry fields; fixed-size name digests and scalar values retain their original memory allowance.

use serde::{de::Visitor, Deserialize, Deserializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use uqa_core::memory::{BudgetedVec, MemoryError};

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

#[cfg(test)]
mod tests;

#[derive(Default, Deserialize)]
struct Fields<'a> {
    #[serde(default, borrow, deserialize_with = "field")]
    labels: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "field")]
    kinds: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "field")]
    dropped_label_ids: Option<&'a RawValue>,
}

fn field<'de, D: Deserializer<'de>>(decoder: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(decoder).map(Some)
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
enum Kind {
    #[serde(rename = "v")]
    Vertex,
    #[serde(rename = "e")]
    Edge,
}

struct Entry<T> {
    name: [u8; 32],
    value: T,
    position: usize,
}

pub(super) struct Registry {
    labels: BudgetedVec<Entry<u32>>,
    kinds: BudgetedVec<Entry<Kind>>,
    dropped: u8,
}

impl Registry {
    pub(super) fn decode(
        source: Option<&str>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Self>> {
        control.check()?;
        let source = source.filter(|source| !source.is_empty()).unwrap_or("{}");
        if !source.trim_start().starts_with('{') {
            return Ok(None);
        }
        // JSON string decoding may retain geometrically grown escape scratch. Borrowed raw fields and scalar visitors allocate no nested JSON tree or decoded name strings.
        let scratch = source
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(16))
            .ok_or(MemoryError::SizeOverflow)?;
        let _scratch = control.memory().reserve(scratch)?;
        let Ok(fields) = serde_json::from_str::<Fields<'_>>(source) else {
            return Ok(None);
        };
        let Some(labels) = map(fields.labels, control)? else {
            return Ok(None);
        };
        let Some(kinds) = map(fields.kinds, control)? else {
            return Ok(None);
        };
        let mut failure = None;
        let dropped = match fields.dropped_label_ids {
            None => 0,
            Some(value) => {
                let mut decoder = serde_json::Deserializer::from_str(value.get());
                let result = decoder.deserialize_seq(Dropped {
                    control,
                    failure: &mut failure,
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                let Ok(value) = result else { return Ok(None) };
                value
            }
        };
        Ok(Some(Self {
            labels,
            kinds,
            dropped,
        }))
    }

    pub(super) fn dropped(&self, id: u32) -> bool {
        self.dropped & (1 << id) != 0
    }

    /// Opaque catalog APIs can import a named entry at a reserved numeric ID. Consumers of the catalog's ID projection must also see those changes.
    pub(super) fn reserved_alias(&self, id: u32) -> bool {
        self.labels.iter().enumerate().any(|(i, entry)| {
            entry.value == id
                && self
                    .labels
                    .get(i + 1)
                    .is_none_or(|next| next.name != entry.name)
        })
    }

    fn definitions(&self) -> impl Iterator<Item = (&[u8; 32], (u32, Kind))> {
        self.labels.iter().enumerate().filter_map(|(i, entry)| {
            if self
                .labels
                .get(i + 1)
                .is_some_and(|next| next.name == entry.name)
            {
                return None;
            }
            let end = self.kinds.partition_point(|kind| kind.name <= entry.name);
            let kind = end
                .checked_sub(1)
                .and_then(|i| self.kinds.get(i))
                .filter(|kind| kind.name == entry.name)
                .map_or(Kind::Vertex, |kind| kind.value);
            Some((&entry.name, (entry.value, kind)))
        })
    }

    pub(super) fn visit_changes(
        &self,
        other: &Self,
        control: &StorageReadControl,
        mut visit: impl FnMut(&[u8; 32]) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        let mut old = self.definitions().peekable();
        let mut new = other.definitions().peekable();
        while old.peek().is_some() || new.peek().is_some() {
            control.check()?;
            match (old.peek(), new.peek()) {
                (Some(a), Some(b)) if a.0 == b.0 => {
                    if a.1 != b.1 {
                        visit(a.0)?;
                    }
                    old.next();
                    new.next();
                }
                (Some(a), Some(b)) if a.0 < b.0 => {
                    visit(a.0)?;
                    old.next();
                }
                (Some(a), None) => {
                    visit(a.0)?;
                    old.next();
                }
                (_, Some(b)) => {
                    visit(b.0)?;
                    new.next();
                }
                (None, None) => break,
            }
        }
        Ok(())
    }
}

struct Name([u8; 32]);
impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct NameVisitor;
        impl Visitor<'_> for NameVisitor {
            type Value = Name;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a registry name")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Name, E> {
                Ok(Name(Sha256::digest(value.as_bytes()).into()))
            }
        }
        decoder.deserialize_str(NameVisitor)
    }
}

fn map<'de, T: Deserialize<'de>>(
    source: Option<&'de RawValue>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<BudgetedVec<Entry<T>>>> {
    struct Entries<'a, T> {
        values: BudgetedVec<Entry<T>>,
        control: &'a StorageReadControl,
        failure: &'a mut Option<StorageBackendError>,
    }
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Entries<'_, T> {
        type Value = BudgetedVec<Entry<T>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a registry map")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            mut self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            loop {
                if let Err(error) = self.control.check() {
                    *self.failure = Some(error);
                    return Err(serde::de::Error::custom("registry decoding interrupted"));
                }
                let Some((Name(name), value)) = map.next_entry()? else {
                    break;
                };
                let position = self.values.len();
                if let Err(error) = self.values.push(Entry {
                    name,
                    value,
                    position,
                }) {
                    *self.failure = Some(error.into());
                    return Err(serde::de::Error::custom("registry decoding interrupted"));
                }
            }
            // Map deserialization keeps the final duplicate. Retain source order for equal names without allocating a sorting workspace.
            self.values
                .sort_unstable_by_key(|entry| (entry.name, entry.position));
            Ok(self.values)
        }
    }
    let mut failure = None;
    let mut decoder = serde_json::Deserializer::from_str(source.map_or("{}", RawValue::get));
    let result = decoder.deserialize_map(Entries {
        values: BudgetedVec::new(control.memory()),
        control,
        failure: &mut failure,
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(result.ok())
}

struct Dropped<'a> {
    control: &'a StorageReadControl,
    failure: &'a mut Option<StorageBackendError>,
}
impl<'de> Visitor<'de> for Dropped<'_> {
    type Value = u8;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("label identifiers")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<u8, A::Error> {
        let mut dropped = 0;
        loop {
            if let Err(error) = self.control.check() {
                *self.failure = Some(error);
                return Err(serde::de::Error::custom("registry decoding interrupted"));
            }
            let Some(id) = sequence.next_element::<u32>()? else {
                return Ok(dropped);
            };
            if matches!(id, 1 | 2) {
                dropped |= 1 << id;
            }
        }
    }
}
