//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original image keys and identity buffers retain admission through native index ownership.

use super::{key::Key, BTreeMap, Bound, DocId, Value};
use crate::{StorageBackendError, StorageBackendResult};
use uqa_core::memory::{Budgeted, BudgetedVec, OwnedMap};

#[derive(Debug)]
pub(super) enum Ids {
    Native(Vec<DocId>),
    Admitted(BudgetedVec<DocId>),
}

impl Ids {
    fn as_slice(&self) -> &[DocId] {
        match self {
            Self::Native(ids) => ids,
            Self::Admitted(ids) => ids,
        }
    }

    fn write(&mut self) -> &mut Vec<DocId> {
        if let Self::Admitted(ids) = self {
            // The ordinary mutation API owns this independent native buffer. Copies precede retirement of the original image allocation.
            *self = Self::Native(ids.to_vec());
        }
        match self {
            Self::Native(ids) => ids,
            Self::Admitted(_) => unreachable!("native identity buffer"),
        }
    }
}

impl Clone for Ids {
    fn clone(&self) -> Self {
        Self::Native(self.as_slice().to_vec())
    }
}

#[derive(Debug, Clone)]
pub(super) enum Entries {
    Native(BTreeMap<Value, Vec<DocId>>),
    Admitted(OwnedMap<Key, Ids>),
}

impl Default for Entries {
    fn default() -> Self {
        Self::Native(BTreeMap::new())
    }
}

pub(super) type Iter<'a> = Box<dyn Iterator<Item = (&'a Value, &'a [DocId])> + 'a>;

impl Entries {
    pub(super) fn admitted() -> Self {
        Self::Admitted(OwnedMap::new())
    }

    pub(super) fn get(&self, value: &Value) -> Option<&[DocId]> {
        match self {
            Self::Native(entries) => entries.get(value).map(Vec::as_slice),
            Self::Admitted(entries) => entries.get(value).map(Ids::as_slice),
        }
    }

    pub(super) fn get_key_value(&self, value: &Value) -> Option<(&Value, &[DocId])> {
        match self {
            Self::Native(entries) => entries
                .get_key_value(value)
                .map(|(key, ids)| (key, ids.as_slice())),
            Self::Admitted(entries) => entries
                .get_key_value(value)
                .map(|(key, ids)| (key.value(), ids.as_slice())),
        }
    }

    pub(super) fn insert(&mut self, id: DocId, value: Value) {
        let ids = match self {
            Self::Native(entries) => entries.entry(value).or_default(),
            Self::Admitted(entries) => {
                if !entries.contains_key(&value) {
                    entries.insert(Key::Native(value), Ids::Native(vec![id]));
                    return;
                }
                entries.get_mut(&value).expect("native key exists").write()
            }
        };
        if let Err(position) = ids.binary_search(&id) {
            ids.insert(position, id);
        }
    }

    pub(super) fn remove(&mut self, id: DocId, value: &Value) {
        let ids = match self {
            Self::Native(entries) => entries.get_mut(value),
            Self::Admitted(entries) => entries.get_mut(value).map(Ids::write),
        };
        let Some(ids) = ids else { return };
        if let Ok(position) = ids.binary_search(&id) {
            ids.remove(position);
        }
        if ids.is_empty() {
            match self {
                Self::Native(entries) => {
                    entries.remove(value);
                }
                Self::Admitted(entries) => {
                    entries.remove(value);
                }
            }
        }
    }

    pub(super) fn clear(&mut self) {
        match self {
            Self::Native(entries) => entries.clear(),
            Self::Admitted(entries) => entries.clear(),
        }
    }

    pub(super) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Native(entries) => {
                Box::new(entries.iter().map(|(key, ids)| (key, ids.as_slice())))
            }
            Self::Admitted(entries) => Box::new(
                entries
                    .iter()
                    .map(|(key, ids)| (key.value(), ids.as_slice())),
            ),
        }
    }

    pub(super) fn range(&self, (low, high): (Bound<Value>, Bound<Value>)) -> Iter<'_> {
        match self {
            Self::Native(entries) => Box::new(
                entries
                    .range((low, high))
                    .map(|(key, ids)| (key, ids.as_slice())),
            ),
            Self::Admitted(entries) => Box::new(
                entries
                    .range_from(low.as_ref())
                    .take_while(move |(key, _)| match &high {
                        Bound::Unbounded => true,
                        Bound::Included(end) => key.value() <= end,
                        Bound::Excluded(end) => key.value() < end,
                    })
                    .map(|(key, ids)| (key.value(), ids.as_slice())),
            ),
        }
    }

    pub(super) fn insert_admitted(
        &mut self,
        id: DocId,
        value: Budgeted<Value>,
    ) -> StorageBackendResult<()> {
        let Self::Admitted(entries) = self else {
            unreachable!("private image builder owns admitted entries");
        };
        if let Some(ids) = entries.get_mut(&*value) {
            let Ids::Admitted(ids) = ids else {
                unreachable!("private image builder exposes no native mutations");
            };
            if ids.last().is_some_and(|previous| *previous >= id) {
                return Err(StorageBackendError::Other(
                    "image identities are not strictly ordered".into(),
                ));
            }
            ids.push(id)?;
        } else {
            let mut ids = BudgetedVec::new(value.budget());
            ids.push(id)?;
            let memory = value.budget().clone();
            let key = Key::Admitted(value.into_shared()?);
            entries.insert_budgeted(key, Ids::Admitted(ids), &memory)?;
        }
        Ok(())
    }
}
