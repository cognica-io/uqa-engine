//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local row overlays and exact index state.

use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedMap, BudgetedString, BudgetedVec, MemoryReservation},
    DocId, Value,
};
use uqa_sql::SQLError;
use uqa_storage::document_store::{Document, DocumentMetadata, RetainedDocumentFields};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

mod exact;
mod keys;
use exact::CommandExactIndex;
use keys::FieldSet;

#[derive(Clone)]
pub struct CommandStoredDocument {
    pub fields: RetainedDocumentFields,
    pub metadata: DocumentMetadata,
}

impl CommandStoredDocument {
    pub fn new(
        fields: Arc<Document>,
        metadata: DocumentMetadata,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            fields: RetainedDocumentFields::new(fields, control)?,
            metadata,
        })
    }
}

#[derive(Default)]
pub struct CommandMutationOverlay {
    tables: Option<BudgetedMap<String, CommandTableOverlay>>,
}

struct CommandTableOverlay {
    documents: BudgetedMap<DocId, Option<CommandStoredDocument>>,
    exact_indexes: BudgetedMap<FieldSet, CommandExactIndex>,
    _name_memory: MemoryReservation,
}

impl CommandMutationOverlay {
    pub fn documents(
        &self,
        table: &str,
    ) -> Option<&BudgetedMap<DocId, Option<CommandStoredDocument>>> {
        self.tables
            .as_ref()?
            .get(table)
            .map(|table| &table.documents)
    }

    fn bind(
        &mut self,
        control: &StorageReadControl,
    ) -> Result<&mut BudgetedMap<String, CommandTableOverlay>, SQLError> {
        control.check().map_err(resource_error)?;
        let tables = self
            .tables
            .get_or_insert_with(|| BudgetedMap::new(control.memory()));
        if !tables.budget().shares_allowance(control.memory()) {
            return Err(SQLError::Internal(
                "command overlay received a different memory allowance".into(),
            ));
        }
        Ok(tables)
    }

    /// Retain an evaluated row and prepare every cached-key change before publishing any row or index mutation.
    pub fn stage(
        &mut self,
        table: &str,
        id: DocId,
        document: Option<(Arc<Document>, DocumentMetadata)>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let tables = self.bind(control)?;
        let document = document
            .map(|(fields, metadata)| {
                CommandStoredDocument::new(fields, metadata, control).map_err(resource_error)
            })
            .transpose()?;
        if let Some(table) = tables.get_mut(table) {
            return table.stage(id, document, control);
        }
        let mut name = BudgetedString::new(control.memory());
        name.push_str(table).map_err(resource_error)?;
        let (name, names) = name.into_parts();
        let mut rows = CommandTableOverlay {
            documents: BudgetedMap::new(control.memory()),
            exact_indexes: BudgetedMap::new(control.memory()),
            _name_memory: names,
        };
        rows.stage(id, document, control)?;
        let entry = tables.prepare_entry(name, rows).map_err(resource_error)?;
        control.check().map_err(resource_error)?;
        tables.insert_prepared(entry);
        Ok(())
    }

    /// Probe exact canonical keys across command frames; newer replacements and tombstones mask older candidates. Candidate merging retains only the smallest visible identity.
    pub fn find_match(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        presence: crate::query::exact_lookup::FieldPresence,
        control: &StorageReadControl,
    ) -> Result<Option<DocId>, SQLError> {
        use crate::query::exact_lookup::FieldPresence;
        if fields.len() != values.len() {
            return Err(SQLError::Internal(
                "command-overlay exact lookup has mismatched fields and values".into(),
            ));
        }
        control.check().map_err(resource_error)?;
        if overlays
            .iter()
            .all(|overlay| overlay.documents(table).is_none())
        {
            return Ok(None);
        }
        let (fields, key) = keys::lookup_parts(fields, values, control)?;
        for overlay in overlays.iter_mut() {
            let Some(table) = overlay.bind(control)?.get_mut(table) else {
                continue;
            };
            if !table.exact_indexes.contains_key(fields.values()) {
                let names = FieldSet::copy(fields.values().iter().map(String::as_str), control)?;
                let index = CommandExactIndex::build(&table.documents, &names, control)?;
                let entry = table
                    .exact_indexes
                    .prepare_entry(names, index)
                    .map_err(resource_error)?;
                control.check().map_err(resource_error)?;
                table.exact_indexes.insert_prepared(entry);
            }
        }
        let mut found = None;
        for (position, overlay) in overlays.iter().enumerate().rev() {
            let Some(rows) = overlay.tables.as_ref().and_then(|tables| tables.get(table)) else {
                continue;
            };
            let index = rows
                .exact_indexes
                .get(fields.values())
                .expect("prepared exact index");
            let Some(candidates) = index.candidates(key.bytes()) else {
                continue;
            };
            for (&id, ()) in candidates {
                control.check().map_err(resource_error)?;
                if found.is_some_and(|found| id >= found) {
                    break;
                }
                if overlays[position + 1..].iter().any(|overlay| {
                    overlay
                        .documents(table)
                        .is_some_and(|rows| rows.contains_key(&id))
                }) {
                    continue;
                }
                // Full canonical bytes already establish value equality. Only field presence is absent from that representation; checking it does not reparse JSONB or allocate numeric comparison scratch.
                let document = rows
                    .documents
                    .get(&id)
                    .and_then(Option::as_ref)
                    .expect("cached present row");
                if matches!(presence, FieldPresence::Required)
                    && fields
                        .values()
                        .iter()
                        .any(|field| !document.fields.contains_key(field))
                {
                    continue;
                }
                found = Some(id);
                break;
            }
        }
        Ok(found)
    }
}

impl CommandTableOverlay {
    fn stage(
        &mut self,
        id: DocId,
        mut document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let previous = self.documents.get(&id).and_then(Option::as_ref);
        let mut updates = BudgetedVec::new(control.memory());
        for (fields, index) in &self.exact_indexes {
            control.check().map_err(resource_error)?;
            let change = index.prepare(id, previous, document.as_ref(), fields, control)?;
            updates.push(change).map_err(resource_error)?;
        }
        let insertion = if self.documents.contains_key(&id) {
            None
        } else {
            Some(
                self.documents
                    .prepare_entry(id, document.take())
                    .map_err(resource_error)?,
            )
        };
        control.check().map_err(resource_error)?;
        // Every fallible operation precedes publication. Borrow each prepared update in the same immutable field-set order used above; no field-name copies or lookup allocations are needed here.
        let mut changes = updates.iter_mut();
        self.exact_indexes.for_each_mut(|_, index| {
            index.apply(id, changes.next().expect("prepared index change").take());
        });
        if let Some(entry) = insertion {
            self.documents.insert_prepared(entry);
        } else {
            *self.documents.get_mut(&id).expect("existing command row") = document;
        }
        Ok(())
    }
}

fn resource_error(error: impl std::error::Error + Send + Sync + 'static) -> SQLError {
    crate::storage_errors::storage_error(
        "retain command overlay",
        &uqa_storage::StorageBackendError::backend("command overlay", error),
    )
}

#[cfg(test)]
mod tests;
