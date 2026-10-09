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
mod expressions;
pub use expressions::CommandIndexProbe;
mod keys;
mod spilled;
mod staged;
use exact::CommandExactIndex;
use keys::{FieldSet, KeyFields, KeyKind};
pub(crate) use spilled::StagedRow;
use staged::StagedRows;
pub(crate) use staged::{StagedCursor, StagedRowsView};

#[derive(Clone)]
pub struct CommandStoredDocument {
    pub fields: RetainedDocumentFields,
    pub metadata: DocumentMetadata,
    /// Evaluated UNIQUE expression keys, separate from user-visible fields.
    index_values: Option<RetainedDocumentFields>,
    /// A nested command already published this version to transaction storage.
    published: bool,
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
            index_values: None,
            published: false,
        })
    }
}

#[derive(Default)]
pub struct CommandMutationOverlay {
    tables: Option<BudgetedMap<String, CommandTableOverlay>>,
    has_publication: bool,
}

/// Immutable row roots at a transaction or savepoint boundary; cached exact keys are rebuilt after undo.
#[derive(Clone, Default)]
pub struct CommandOverlayCheckpoint {
    tables: Vec<(String, StagedRows)>,
    has_publication: bool,
}

struct CommandTableOverlay {
    rows: StagedRows,
    /// Exact indexes of the rows in memory; the spilled tier holds the entries of its own rows.
    exact_indexes: BudgetedMap<FieldSet, CommandExactIndex>,
    expression_indexes: BudgetedMap<FieldSet, CommandExactIndex>,
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

    /// At least the number of rows `overlays` staged for `table`, without reading them; a row staged by several commands or staged again after it spilled counts more than once.
    pub fn staged_row_bound(overlays: &[Self], table: &str) -> u64 {
        overlays
            .iter()
            .filter_map(|overlay| overlay.table(table))
            .fold(0, |count, rows| {
                count.saturating_add(rows.rows.count_bound())
            })
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
        let document = document
            .map(|(fields, metadata)| CommandStoredDocument::new(fields, metadata, control))
            .transpose()
            .map_err(resource_error)?;
        self.stage_evaluated(table, id, document, control)
    }

    /// Publish an already evaluated row and its expression keys together.
    pub fn stage_evaluated(
        &mut self,
        table: &str,
        id: DocId,
        document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let tables = self.bind(control)?;
        if let Some(table) = tables.get_mut(table) {
            return table.stage(id, document, control);
        }
        let mut name = BudgetedString::new(control.memory());
        name.push_str(table).map_err(resource_error)?;
        let (name, names) = name.into_parts();
        let mut rows = CommandTableOverlay {
            rows: StagedRows::new(control),
            exact_indexes: BudgetedMap::new(control.memory()),
            expression_indexes: BudgetedMap::new(control.memory()),
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
        Self::find_match_with_catalog(overlays, table, fields, values, presence, control, None)
    }

    /// Retain catalog-dependent comparison semantics while newer frames mask older rows.
    pub fn find_match_with_catalog(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        presence: FieldPresence,
        control: &StorageReadControl,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
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
            return comparison::find_match(
                overlays, table, fields, values, presence, control, catalog,
            );
        }
        let (fields, key) = keys::lookup_parts(fields, values, control)?;
        for overlay in overlays.iter_mut() {
            if let Some(rows) = overlay.bind(control)?.get_mut(table) {
                rows.prepare_index(KeyKind::Columns, &fields, control)?;
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
    pub fn checkpoint(&self) -> CommandOverlayCheckpoint {
        CommandOverlayCheckpoint {
            tables: self
                .tables
                .iter()
                .flat_map(|tables| tables.iter())
                .map(|(name, table)| (name.clone(), table.rows.clone()))
                .collect(),
            has_publication: self.has_publication,
        }
    }

    pub fn restore(&mut self, checkpoint: &CommandOverlayCheckpoint) {
        if let Some(tables) = self.tables.as_mut() {
            tables.for_each_mut(|name, table| {
                table.rows = checkpoint
                    .tables
                    .iter()
                    .find(|(saved, _)| saved == name)
                    .map_or_else(
                        || {
                            StagedRows::new(&StorageReadControl::new(
                                table.rows.memory.budget(),
                                &uqa_core::CancellationToken::new(),
                            ))
                        },
                        |(_, rows)| rows.clone(),
                    );
                table.exact_indexes = BudgetedMap::new(table.exact_indexes.budget());
                table.expression_indexes = BudgetedMap::new(table.expression_indexes.budget());
                // Index ordinals remain monotone: restored runs can still contain entries of an earlier cached index.
            });
        }
        self.has_publication = checkpoint.has_publication;
    }

    /// Replace every enclosing command's staged image after a nested command writes it. This keeps later callbacks and exact-key probes on the newest version.
    pub fn published(
        overlays: &mut [Self],
        table: &str,
        id: DocId,
        document: Option<(Arc<Document>, DocumentMetadata)>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let document = document
            .map(|(fields, metadata)| CommandStoredDocument::new(fields, metadata, control))
            .transpose()
            .map_err(resource_error)?;
        Self::published_evaluated(overlays, table, id, document, control)
    }

    /// Refresh enclosing rows with the keys evaluated for their published image.
    pub fn published_evaluated(
        overlays: &mut [Self],
        table: &str,
        id: DocId,
        mut document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        if let Some(document) = document.as_mut() {
            document.published = true;
        }
        for overlay in overlays {
            if Self::stages(std::slice::from_ref(overlay), table, id, control)? {
                overlay.stage_evaluated(table, id, document.clone(), control)?;
                overlay.has_publication = true;
            }
        }
        Ok(())
    }

    /// Retain an ended command only when a nested command changed one of its staged rows.
    pub fn published_rows(self) -> Option<Self> {
        self.has_publication.then_some(self)
    }

    /// Whether writing an identity would overwrite a nested command's published row, including its deletion when `deleted_supersedes` holds.
    pub fn was_published(
        &self,
        table: &str,
        id: DocId,
        deleted_supersedes: bool,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<bool, SQLError> {
        let Some(table) = self.table(table) else {
            return Ok(false);
        };
        let control = StorageReadControl::new(table.rows.memory.budget(), cancellation);
        Ok(table
            .rows
            .get(id, &control)
            .map_err(storage_error)?
            .is_some_and(|row| row.map_or(deleted_supersedes, |row| row.published)))
    }

    /// The visible rows `overlays` staged for `table` whose `fields` hold `values`, the newest command's first; a missing field holds null.
    pub fn matches(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        control: &StorageReadControl,
    ) -> Result<BudgetedVec<DocId>, SQLError> {
        Self::matches_with_catalog(overlays, table, fields, values, control, None)
    }

    pub fn matches_with_catalog(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        control: &StorageReadControl,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> Result<BudgetedVec<DocId>, SQLError> {
        Self::matches_keys(
            overlays,
            table,
            fields,
            values,
            KeyKind::Columns,
            control,
            catalog,
        )
    }

    fn matches_keys(
        overlays: &mut [Self],
        table: &str,
        fields: &[String],
        values: &[Value],
        kind: KeyKind,
        control: &StorageReadControl,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
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
            return comparison::matches(overlays, table, fields, values, kind, control, catalog);
        }
        let (fields, key) = keys::lookup_parts(fields, values, control)?;
        for overlay in overlays.iter_mut() {
            if let Some(rows) = overlay.bind(control)?.get_mut(table) {
                rows.prepare_index(kind, &fields, control)?;
            }
        }
        for (position, overlay) in overlays.iter().enumerate().rev() {
            let Some(rows) = overlay.table(table) else {
                continue;
            };
            let newer = &overlays[position + 1..];
            let index = rows
                .indexes(kind)
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

mod table;

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
