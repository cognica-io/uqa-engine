//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact cache changes reserve complete key and identity nodes before publishing a staged row.

use super::{
    keys::{document_key, ExactKey, FieldSet},
    resource_error, CommandStoredDocument, DocId, SQLError, StorageReadControl,
};
use uqa_core::memory::{BudgetedMap, PreparedMapEntry};

pub(super) struct CommandExactIndex {
    doc_ids_by_key: BudgetedMap<ExactKey, KeyRows>,
}

struct KeyRows {
    ids: BudgetedMap<DocId, ()>,
}

pub(super) struct PreparedChange {
    previous: Option<ExactKey>,
    replacement: Option<Replacement>,
}

enum Replacement {
    Existing {
        key: ExactKey,
        entry: PreparedMapEntry<DocId, ()>,
    },
    New(PreparedMapEntry<ExactKey, KeyRows>),
}

impl CommandExactIndex {
    pub(super) fn build(
        documents: &BudgetedMap<DocId, Option<CommandStoredDocument>>,
        fields: &FieldSet,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        let mut index = Self {
            doc_ids_by_key: BudgetedMap::new(control.memory()),
        };
        for (id, document) in documents {
            control.check().map_err(resource_error)?;
            let Some(document) = document else { continue };
            let change = index.prepare(*id, None, Some(document), fields, control)?;
            index.apply(*id, change);
        }
        Ok(index)
    }

    pub(super) fn candidates(&self, key: &[u8]) -> Option<&BudgetedMap<DocId, ()>> {
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
        let replacement = replacement
            .map(|key| {
                if let Some(rows) = self.doc_ids_by_key.get(key.bytes()) {
                    let entry = rows.ids.prepare_entry(id, ()).map_err(resource_error)?;
                    Ok(Replacement::Existing { key, entry })
                } else {
                    let mut rows = KeyRows {
                        ids: BudgetedMap::new(control.memory()),
                    };
                    rows.ids.insert(id, ()).map_err(resource_error)?;
                    self.doc_ids_by_key
                        .prepare_entry(key, rows)
                        .map(Replacement::New)
                        .map_err(resource_error)
                }
            })
            .transpose()?;
        Ok(Some(PreparedChange {
            previous,
            replacement,
        }))
    }

    pub(super) fn apply(&mut self, id: DocId, change: Option<PreparedChange>) {
        let Some(change) = change else { return };
        if let Some(key) = change.previous {
            let rows = self
                .doc_ids_by_key
                .get_mut(key.bytes())
                .expect("cached previous key");
            assert!(
                rows.ids.remove(&id).is_some(),
                "cached previous document identity"
            );
            if rows.ids.is_empty() {
                self.doc_ids_by_key.remove(key.bytes());
            }
        }
        match change.replacement {
            Some(Replacement::Existing { key, entry }) => {
                self.doc_ids_by_key
                    .get_mut(key.bytes())
                    .expect("prepared existing key")
                    .ids
                    .insert_prepared(entry);
            }
            Some(Replacement::New(entry)) => {
                self.doc_ids_by_key.insert_prepared(entry);
            }
            None => {}
        }
    }
}
