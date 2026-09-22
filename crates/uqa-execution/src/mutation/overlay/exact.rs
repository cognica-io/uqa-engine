//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact cache changes are fully prepared before publishing a staged row.

use super::{
    keys::{document_key, ExactKey, FieldSet},
    resource_error, CommandStoredDocument, DocId, SQLError, StorageReadControl,
};
use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};
use uqa_core::memory::MemoryReservation;

#[derive(Default)]
pub(super) struct CommandExactIndex {
    doc_ids_by_key: BTreeMap<ExactKey, KeyRows>,
}

struct KeyRows {
    ids: BTreeSet<DocId>,
    // Live map/set entries are charged; opaque standard-library node slack has its own accounting boundary.
    memory: MemoryReservation,
}

pub(super) struct PreparedChange {
    previous: Option<ExactKey>,
    replacement: Option<ExactKey>,
    memory: MemoryReservation,
}

impl CommandExactIndex {
    pub(super) fn build(
        documents: &BTreeMap<DocId, Option<CommandStoredDocument>>,
        fields: &FieldSet,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        let mut index = Self::default();
        for (id, document) in documents {
            control.check().map_err(resource_error)?;
            let Some(document) = document else { continue };
            let change = index.prepare(*id, None, Some(document), fields, control)?;
            index.apply(*id, change);
        }
        Ok(index)
    }

    pub(super) fn candidates(&self, key: &[u8]) -> Option<&BTreeSet<DocId>> {
        self.doc_ids_by_key.get(key).map(|rows| &rows.ids)
    }

    pub(super) fn prepare(
        &self,
        id: DocId,
        previous: Option<&CommandStoredDocument>,
        replacement: Option<&CommandStoredDocument>,
        fields: &FieldSet,
        control: &StorageReadControl,
    ) -> Result<Option<PreparedChange>, SQLError> {
        let previous = previous
            .map(|document| document_key(document.fields.as_ref(), fields, control))
            .transpose()?;
        let replacement = replacement
            .map(|document| document_key(document.fields.as_ref(), fields, control))
            .transpose()?;
        if previous == replacement {
            return Ok(None);
        }
        let bytes =
            replacement
                .as_ref()
                .map_or(0, |key| match self.doc_ids_by_key.get(key.bytes()) {
                    Some(rows) if rows.ids.contains(&id) => 0,
                    Some(_) => size_of::<DocId>(),
                    None => size_of::<(ExactKey, KeyRows)>() + size_of::<DocId>(),
                });
        Ok(Some(PreparedChange {
            previous,
            replacement,
            memory: control.memory().reserve(bytes).map_err(resource_error)?,
        }))
    }

    pub(super) fn apply(&mut self, id: DocId, change: Option<PreparedChange>) {
        let Some(change) = change else { return };
        let PreparedChange {
            previous,
            replacement,
            memory,
        } = change;
        if let Some(key) = previous {
            let rows = self
                .doc_ids_by_key
                .get_mut(key.bytes())
                .expect("cached previous key");
            let removed = rows.ids.remove(&id);
            assert!(removed, "cached previous document identity");
            drop(rows.memory.split(size_of::<DocId>()));
            if rows.ids.is_empty() {
                self.doc_ids_by_key.remove(key.bytes());
            }
        }
        if let Some(key) = replacement {
            match self.doc_ids_by_key.entry(key) {
                Entry::Occupied(mut entry) => {
                    let rows = entry.get_mut();
                    rows.memory.absorb(memory);
                    rows.ids.insert(id);
                }
                Entry::Vacant(entry) => {
                    entry.insert(KeyRows {
                        ids: BTreeSet::from([id]),
                        memory,
                    });
                }
            }
        }
    }
}
