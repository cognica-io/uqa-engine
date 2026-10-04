//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local row overlays and exact index state. The rows a command stages for a table spill to encrypted temporary files once its allowance is under pressure, as a transaction's own changes do, so that one statement is bounded by disk rather than by its allowance.

use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedMap, BudgetedString, BudgetedVec, MemoryReservation},
    DocId, Value,
};
use uqa_sql::SQLError;
use uqa_storage::document_store::{Document, DocumentMetadata, RetainedDocumentFields};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

use crate::query::document_changes::DocumentChanges;
use crate::query::exact_lookup::FieldPresence;

mod comparison;
mod exact;
mod keys;
mod spilled;
mod staged;
use exact::CommandExactIndex;
use keys::FieldSet;
pub(crate) use spilled::StagedRow;
use staged::StagedRows;
pub(crate) use staged::{StagedCursor, StagedRowsView};

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
    rows: StagedRows,
    /// Exact indexes of the rows in memory; the spilled tier holds the entries of its own rows.
    exact_indexes: BudgetedMap<FieldSet, CommandExactIndex>,
    /// The ordinal the next exact index takes in the spilled tier. Ordinals are never reused, so the entries an index whose construction failed left behind are never read.
    next_ordinal: u32,
    has_fallible_comparison: bool,
    _name_memory: MemoryReservation,
}

impl CommandMutationOverlay {
    fn table(&self, table: &str) -> Option<&CommandTableOverlay> {
        self.tables.as_ref()?.get(table)
    }

    /// Whether this command staged a row for `table`.
    pub fn holds(&self, table: &str) -> bool {
        self.table(table)
            .is_some_and(|table| !table.rows.is_empty())
    }

    /// The row the newest of `overlays` staged for `id` in `table`: its fields, or `None` for a row the command deleted. `None` when no command staged the row.
    pub fn row(
        overlays: &[Self],
        table: &str,
        id: DocId,
        control: &StorageReadControl,
    ) -> Result<Option<Option<CommandStoredDocument>>, SQLError> {
        for overlay in overlays.iter().rev() {
            if let Some(rows) = overlay.table(table) {
                if let Some(row) = rows.rows.get(id, control).map_err(storage_error)? {
                    return Ok(Some(row));
                }
            }
        }
        Ok(None)
    }

    /// Whether one of `overlays` staged a row for `id` in `table`.
    pub fn stages(
        overlays: &[Self],
        table: &str,
        id: DocId,
        control: &StorageReadControl,
    ) -> Result<bool, SQLError> {
        for overlay in overlays {
            if let Some(rows) = overlay.table(table) {
                if rows.rows.contains(id, control).map_err(storage_error)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// `base` with the rows `overlays` staged for `table` above it, the newest command's rows above the others. The rows are not copied; later stages do not change the result.
    pub fn changes(
        overlays: &[Self],
        table: &str,
        base: DocumentChanges,
        control: &StorageReadControl,
    ) -> Result<DocumentChanges, SQLError> {
        let views = overlays
            .iter()
            .filter_map(|overlay| overlay.table(table))
            .filter(|rows| !rows.rows.is_empty())
            .map(|rows| rows.rows.view());
        base.with_staged(views, control).map_err(storage_error)
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
            rows: StagedRows::new(control),
            exact_indexes: BudgetedMap::new(control.memory()),
            next_ordinal: 0,
            has_fallible_comparison: false,
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
        presence: FieldPresence,
        control: &StorageReadControl,
    ) -> Result<Option<DocId>, SQLError> {
        if fields.len() != values.len() {
            return Err(SQLError::Internal(
                "command-overlay exact lookup has mismatched fields and values".into(),
            ));
        }
        control.check().map_err(resource_error)?;
        if overlays.iter().all(|overlay| !overlay.holds(table)) {
            return Ok(None);
        }
        if values.iter().any(uqa_sql::expr::value_comparison_can_fail)
            || overlays.iter().any(|overlay| {
                overlay
                    .table(table)
                    .is_some_and(|table| table.has_fallible_comparison)
            })
        {
            return comparison::find_match(overlays, table, fields, values, presence, control);
        }
        let (fields, key) = keys::lookup_parts(fields, values, control)?;
        for overlay in overlays.iter_mut() {
            if let Some(rows) = overlay.bind(control)?.get_mut(table) {
                rows.prepare_index(&fields, control)?;
            }
        }
        let mut found = None;
        for (position, overlay) in overlays.iter().enumerate().rev() {
            let Some(rows) = overlay.table(table) else {
                continue;
            };
            let newer = &overlays[position + 1..];
            let index = rows
                .exact_indexes
                .get(fields.values())
                .expect("prepared exact index");
            if let Some(candidates) = index.candidates(key.bytes()) {
                for (&id, ()) in candidates {
                    control.check().map_err(resource_error)?;
                    if found.is_some_and(|found| id >= found) {
                        break;
                    }
                    if Self::stages(newer, table, id, control)? {
                        continue;
                    }
                    // Full canonical bytes already establish value equality. Only field presence is absent from that representation; checking it does not reparse JSONB or allocate numeric comparison scratch.
                    let document = rows
                        .rows
                        .memory
                        .get(&id)
                        .and_then(Option::as_ref)
                        .expect("cached present row");
                    if matches!(presence, FieldPresence::Required)
                        && !holds_fields(document, &fields)
                    {
                        continue;
                    }
                    found = Some(id);
                    break;
                }
            }
            if let Some(spilled) = &rows.rows.spilled {
                let mut after = None;
                'pages: loop {
                    let page = spilled::index_page(
                        spilled.view(),
                        index.ordinal(),
                        key.bytes(),
                        after,
                        control,
                    )
                    .map_err(storage_error)?;
                    for (id, complete) in page.rows.iter().copied() {
                        control.check().map_err(resource_error)?;
                        if found.is_some_and(|found| id >= found) {
                            break 'pages;
                        }
                        // A row in memory shadows its spilled version, whose entries the memory tier's index replaces.
                        if rows.rows.memory.get(&id).is_some()
                            || Self::stages(newer, table, id, control)?
                            || (matches!(presence, FieldPresence::Required) && !complete)
                        {
                            continue;
                        }
                        found = Some(id);
                        break 'pages;
                    }
                    match page.resume {
                        Some(resume) => after = Some(resume),
                        None => break,
                    }
                }
            }
        }
        Ok(found)
    }
}

impl CommandMutationOverlay {
    /// The visible rows `overlays` staged for `table` whose `fields` hold `values`, the newest command's first; a missing field holds null.
    pub fn matches(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        control: &StorageReadControl,
    ) -> Result<BudgetedVec<DocId>, SQLError> {
        if fields.len() != values.len() {
            return Err(SQLError::Internal(
                "command-overlay exact lookup has mismatched fields and values".into(),
            ));
        }
        control.check().map_err(resource_error)?;
        let mut found = BudgetedVec::new(control.memory());
        if overlays.iter().all(|overlay| !overlay.holds(table)) {
            return Ok(found);
        }
        if values.iter().any(uqa_sql::expr::value_comparison_can_fail)
            || overlays.iter().any(|overlay| {
                overlay
                    .table(table)
                    .is_some_and(|table| table.has_fallible_comparison)
            })
        {
            return comparison::matches(overlays, table, fields, values, control);
        }
        let (fields, key) = keys::lookup_parts(fields, values, control)?;
        for overlay in overlays.iter_mut() {
            if let Some(rows) = overlay.bind(control)?.get_mut(table) {
                rows.prepare_index(&fields, control)?;
            }
        }
        for (position, overlay) in overlays.iter().enumerate().rev() {
            let Some(rows) = overlay.table(table) else {
                continue;
            };
            let newer = &overlays[position + 1..];
            let index = rows
                .exact_indexes
                .get(fields.values())
                .expect("prepared exact index");
            for (&id, ()) in index.candidates(key.bytes()).into_iter().flatten() {
                control.check().map_err(resource_error)?;
                if !Self::stages(newer, table, id, control)? {
                    found.push(id).map_err(resource_error)?;
                }
            }
            let Some(spilled) = &rows.rows.spilled else {
                continue;
            };
            let mut after = None;
            loop {
                let page = spilled::index_page(
                    spilled.view(),
                    index.ordinal(),
                    key.bytes(),
                    after,
                    control,
                )
                .map_err(storage_error)?;
                for (id, _) in page.rows.iter().copied() {
                    control.check().map_err(resource_error)?;
                    if rows.rows.memory.get(&id).is_none()
                        && !Self::stages(newer, table, id, control)?
                    {
                        found.push(id).map_err(resource_error)?;
                    }
                }
                match page.resume {
                    Some(resume) => after = Some(resume),
                    None => break,
                }
            }
        }
        Ok(found)
    }
}

/// Whether `document` holds every field of `fields`.
fn holds_fields(document: &CommandStoredDocument, fields: &FieldSet) -> bool {
    fields
        .values()
        .iter()
        .all(|field| document.fields.contains_key(field))
}

impl CommandTableOverlay {
    fn stage(
        &mut self,
        id: DocId,
        document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        if self.rows.needs_room(control) {
            self.spill(control)?;
        }
        let has_fallible_comparison = document.as_ref().is_some_and(|document| {
            document
                .fields
                .values()
                .any(uqa_sql::expr::value_comparison_can_fail)
        });
        // The memory tier's indexes hold its own rows; a spilled version that this row replaces keeps its entries in the spilled tier, where the row in memory shadows them.
        let previous = self.rows.memory.get(&id).and_then(Option::as_ref);
        let mut updates = BudgetedVec::new(control.memory());
        for (fields, index) in &self.exact_indexes {
            control.check().map_err(resource_error)?;
            let change = index.prepare(id, previous, document.as_ref(), fields, control)?;
            updates.push(change).map_err(resource_error)?;
        }
        control.check().map_err(resource_error)?;
        self.rows.insert(id, document).map_err(storage_error)?;
        // Every fallible operation precedes publication. Borrow each prepared update in the same immutable field-set order used above; no field-name copies or lookup allocations are needed here.
        self.has_fallible_comparison |= has_fallible_comparison;
        let mut changes = updates.iter_mut();
        self.exact_indexes.for_each_mut(|_, index| {
            index.apply(id, changes.next().expect("prepared index change").take());
        });
        Ok(())
    }

    /// Move every row in memory, with its exact index entries, into the spilled tier. Failure leaves both tiers unchanged.
    fn spill(&mut self, control: &StorageReadControl) -> Result<(), SQLError> {
        if self.rows.spilled.is_none() {
            self.rows.spilled = Some(spilled::SpilledRows::new(control).map_err(storage_error)?);
        }
        let memory = &self.rows.memory;
        let indexes = &self.exact_indexes;
        let spilled = self.rows.spilled.as_mut().expect("a spilled tier");
        let previous = Arc::clone(spilled.view());
        spilled.transact(control, |writer| {
            let mut batch = Vec::new();
            let mut counts = std::collections::BTreeMap::new();
            for (&id, row) in memory {
                control.check().map_err(resource_error)?;
                // The entries of the version this row replaces leave the spilled tier with it.
                let replaced = if indexes.is_empty() {
                    None
                } else {
                    spilled::row(&previous, id, control)
                        .map_err(storage_error)?
                        .flatten()
                };
                batch.push((
                    spilled::row_key(id),
                    row.as_ref()
                        .map(spilled::encode_row)
                        .transpose()
                        .map_err(storage_error)?,
                ));
                for (fields, index) in indexes {
                    let ordinal = index.ordinal();
                    let old = replaced
                        .as_ref()
                        .map(|old| keys::document_key(old.fields.as_ref(), fields, control))
                        .transpose()?;
                    let new = row
                        .as_ref()
                        .map(|row| keys::document_key(row.fields.as_ref(), fields, control))
                        .transpose()?;
                    if old != new {
                        if let Some(old) = &old {
                            batch.push((
                                spilled::index_key(ordinal, old.bytes(), id)
                                    .map_err(storage_error)?,
                                None,
                            ));
                            *counts
                                .entry(
                                    spilled::key_record(ordinal, old.bytes())
                                        .map_err(storage_error)?,
                                )
                                .or_insert(0) -= 1;
                        }
                        if let Some(new) = &new {
                            *counts
                                .entry(
                                    spilled::key_record(ordinal, new.bytes())
                                        .map_err(storage_error)?,
                                )
                                .or_insert(0) += 1;
                        }
                    }
                    if let (Some(row), Some(new)) = (row, &new) {
                        batch.push((
                            spilled::index_key(ordinal, new.bytes(), id).map_err(storage_error)?,
                            Some(spilled::index_value(holds_fields(row, fields))),
                        ));
                    }
                }
                if batch.len() >= spilled::PAGE_RECORDS {
                    writer.write(&mut batch, &mut counts)?;
                }
            }
            writer.write(&mut batch, &mut counts)
        })?;
        self.rows.clear_memory();
        self.exact_indexes.for_each_mut(|_, index| index.clear());
        Ok(())
    }

    /// Build the exact index of `fields` when the table has none: over the rows in memory, and as entries of the spilled tier over its rows.
    fn prepare_index(
        &mut self,
        fields: &FieldSet,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        if self.exact_indexes.contains_key(fields.values()) {
            return Ok(());
        }
        let names = FieldSet::copy(fields.values().iter().map(String::as_str), control)?;
        let ordinal = self.next_ordinal;
        self.next_ordinal = ordinal.checked_add(1).ok_or_else(|| {
            SQLError::Internal("command exact index ordinals are exhausted".into())
        })?;
        let index = CommandExactIndex::build(&self.rows.memory, &names, ordinal, control)?;
        if let Some(spilled) = self.rows.spilled.as_mut() {
            let view = Arc::clone(spilled.view());
            spilled.transact(control, |writer| {
                let mut after = None;
                let mut batch = Vec::new();
                let mut counts = std::collections::BTreeMap::new();
                loop {
                    let page = spilled::row_page(&view, after, control).map_err(storage_error)?;
                    for (id, row) in page.rows.iter() {
                        let Some(row) = row else { continue };
                        let key = keys::document_key(row.fields.as_ref(), &names, control)?;
                        batch.push((
                            spilled::index_key(ordinal, key.bytes(), *id).map_err(storage_error)?,
                            Some(spilled::index_value(holds_fields(row, &names))),
                        ));
                        *counts
                            .entry(
                                spilled::key_record(ordinal, key.bytes()).map_err(storage_error)?,
                            )
                            .or_insert(0) += 1;
                    }
                    writer.write(&mut batch, &mut counts)?;
                    match page.resume {
                        Some(resume) => after = Some(resume),
                        None => return Ok(()),
                    }
                }
            })?;
        }
        let entry = self
            .exact_indexes
            .prepare_entry(names, index)
            .map_err(resource_error)?;
        control.check().map_err(resource_error)?;
        self.exact_indexes.insert_prepared(entry);
        Ok(())
    }
}

fn resource_error(error: impl std::error::Error + Send + Sync + 'static) -> SQLError {
    crate::storage_errors::storage_error(
        "retain command overlay",
        &uqa_storage::StorageBackendError::backend("command overlay", error),
    )
}

fn storage_error(error: uqa_storage::StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error("retain command overlay", &error)
}

#[cfg(test)]
mod tests;
