//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local row overlays and exact index state.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedString, BudgetedVec, MemoryReservation},
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
    tables: BTreeMap<String, CommandTableOverlay>,
    memory: Option<MemoryReservation>,
}

struct CommandTableOverlay {
    documents: BTreeMap<DocId, Option<CommandStoredDocument>>,
    exact_indexes: BTreeMap<FieldSet, CommandExactIndex>,
    document_memory: MemoryReservation,
}

impl CommandMutationOverlay {
    pub fn documents(
        &self,
        table: &str,
    ) -> Option<&BTreeMap<DocId, Option<CommandStoredDocument>>> {
        self.tables.get(table).map(|table| &table.documents)
    }

    fn bind(&mut self, control: &StorageReadControl) -> Result<(), SQLError> {
        control.check().map_err(resource_error)?;
        match &self.memory {
            Some(memory) if !memory.budget().shares_allowance(control.memory()) => Err(
                SQLError::Internal("command overlay received a different memory allowance".into()),
            ),
            Some(_) => Ok(()),
            None => {
                self.memory = Some(control.memory().empty_reservation());
                Ok(())
            }
        }
    }

    /// Retain an evaluated row and prepare every cached-key change before publishing any row or index mutation.
    pub fn stage(
        &mut self,
        table: &str,
        id: DocId,
        document: Option<(Arc<Document>, DocumentMetadata)>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        self.bind(control)?;
        let document = document
            .map(|(fields, metadata)| {
                CommandStoredDocument::new(fields, metadata, control).map_err(resource_error)
            })
            .transpose()?;
        if let Some(table) = self.tables.get_mut(table) {
            return table.stage(id, document, control);
        }
        let mut name = BudgetedString::new(control.memory());
        name.push_str(table).map_err(resource_error)?;
        let mut entry = control
            .memory()
            .reserve(size_of::<(String, CommandTableOverlay)>())
            .map_err(resource_error)?;
        let mut rows = CommandTableOverlay {
            documents: BTreeMap::new(),
            exact_indexes: BTreeMap::new(),
            document_memory: control.memory().empty_reservation(),
        };
        rows.stage(id, document, control)?;
        let (name, names) = name.into_parts();
        entry.absorb(names);
        self.tables.insert(name, rows);
        self.memory.as_mut().expect("bound overlay").absorb(entry);
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
            overlay.bind(control)?;
            let Some(table) = overlay.tables.get_mut(table) else {
                continue;
            };
            if !table.exact_indexes.contains_key(fields.values()) {
                let mut names =
                    FieldSet::copy(fields.values().iter().map(String::as_str), control)?;
                names.reserve_index_entry()?;
                let index = CommandExactIndex::build(&table.documents, &names, control)?;
                control.check().map_err(resource_error)?;
                table.exact_indexes.insert(names, index);
            }
        }
        let mut found = None;
        for (position, overlay) in overlays.iter().enumerate().rev() {
            let Some(rows) = overlay.tables.get(table) else {
                continue;
            };
            let index = rows
                .exact_indexes
                .get(fields.values())
                .expect("prepared exact index");
            let Some(candidates) = index.candidates(key.bytes()) else {
                continue;
            };
            for &id in candidates {
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
        document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let previous = self.documents.get(&id).and_then(Option::as_ref);
        let mut updates = BudgetedVec::new(control.memory());
        for (fields, index) in &self.exact_indexes {
            control.check().map_err(resource_error)?;
            let change = index.prepare(id, previous, document.as_ref(), fields, control)?;
            updates.push(change).map_err(resource_error)?;
        }
        let retained = control
            .memory()
            .reserve(if self.documents.contains_key(&id) {
                0
            } else {
                size_of::<(DocId, Option<CommandStoredDocument>)>()
            })
            .map_err(resource_error)?;
        control.check().map_err(resource_error)?;
        // Every fallible operation precedes publication. Borrow each prepared update in the same immutable field-set order used above; no field-name copies or lookup allocations are needed here.
        for (index, update) in self.exact_indexes.values_mut().zip(updates.iter_mut()) {
            index.apply(id, update.take());
        }
        self.documents.insert(id, document);
        self.document_memory.absorb(retained);
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
