//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private rows retain the physical canonical sources captured with their evaluated versions.

use super::{
    Arc, Change, DocId, DocumentChanges, DocumentSelection, DocumentStore, StorageBackendResult,
    StorageReadControl,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{
    diskann_index::{DiskANNReadChanges, DiskANNReadSnapshot},
    vector_index::{
        RetainedVectorIndexesBuilder, SelectedVectorRead, VectorIndexSource, VectorReadSnapshot,
    },
};

struct VectorSource {
    field: String,
    column: Option<[u8; 16]>,
    source: Option<DiskANNReadSnapshot>,
    values: VectorReadSnapshot,
    _memory: MemoryReservation,
}

/// The vector sources the indexes of a relation hold now, beside its rows `documents`; `None` when no index holds vectors.
fn capture_vectors(
    documents: &Arc<dyn DocumentStore>,
    columns: &[ColumnDef],
    indexes: &dyn VectorIndexSource,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Arc<Budgeted<CapturedRows>>>> {
    let mut vectors = BudgetedVec::new(control.memory());
    indexes.visit(&mut |field, index| {
        let source = index.diskann_read_snapshot(control)?;
        let values = if let Some(source) = &source {
            Some(
                Budgeted::new(source.clone(), control.memory().empty_reservation()).into_shared()?
                    as VectorReadSnapshot,
            )
        } else {
            index.vector_read_snapshot(control)?
        };
        let Some(values) = values else {
            return Ok(());
        };
        vectors.reserve(1)?;
        let column = columns
            .iter()
            .find(|column| column.name == field)
            .and_then(|column| column.object_id);
        let (field, memory) = RetainedVectorIndexesBuilder::copy_field(field, control)?;
        vectors.push(VectorSource {
            field,
            column,
            source,
            values,
            _memory: memory,
        })?;
        Ok(())
    })?;
    if vectors.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        Budgeted::new(
            CapturedRows {
                documents: Arc::clone(documents),
                vectors,
            },
            control.memory().empty_reservation(),
        )
        .into_shared()?,
    ))
}

pub(super) struct CapturedRows {
    pub(super) documents: Arc<dyn DocumentStore>,
    vectors: BudgetedVec<VectorSource>,
}

impl CapturedRows {
    fn vector(&self, field: &str, column: Option<&ColumnDef>) -> Option<&VectorSource> {
        let identity = column.and_then(|column| column.object_id);
        self.vectors
            .iter()
            .find(|source| match (source.column, identity) {
                (Some(source), Some(target)) => source == target,
                _ => source.field == field,
            })
    }
}

impl DocumentChanges {
    /// Capture immutable rows and their actual canonical vector sources at the same selected table boundary. Each retained row, including a tombstone, keeps the source that supplied that version across later captures, schema renames and transaction completion. Raw sources carry values independently of physical mutation provenance.
    pub fn with_retained_vectors(
        mut self,
        documents: Arc<dyn DocumentStore>,
        desired: DocumentSelection,
        columns: &[ColumnDef],
        indexes: &dyn VectorIndexSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let Some(source) = capture_vectors(&documents, columns, indexes, control)? else {
            return self.with_retained(documents, desired, control);
        };
        let desired = desired.finish(control)?;
        let mut newer = Self::default();
        for (id, present) in desired.entries() {
            control.check()?;
            let present = present && source.documents.contains_doc_id(id)?;
            newer.insert(id, Change::Captured(source.clone(), present), control)?;
        }
        self.extend(newer, control)?;
        Ok(self)
    }

    /// These changes with the rows a transaction changed above them, as `changed` records them: each present one read from `documents`, the transaction's view of the relation, with the vector sources it holds now.
    pub fn with_identities(
        self,
        changed: super::ChangedIdentitiesView,
        documents: Arc<dyn DocumentStore>,
        columns: &[ColumnDef],
        indexes: &dyn VectorIndexSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let rows = match capture_vectors(&documents, columns, indexes, control)? {
            Some(source) => super::layers::IdentityRows::Captured(source),
            None => super::layers::IdentityRows::Retained(documents),
        };
        self.with_identity_layer(changed, rows, control)
    }

    /// Select the actual source retained with each evaluated private row. None means this row source predates physical capture or has no matching column incarnation; callers must not substitute a later live source.
    pub(in crate::query) fn diskann_read_changes(
        &self,
        field: &str,
        column: Option<&ColumnDef>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNReadChanges>> {
        let physical = |change: &Change| matches!(change, Change::Captured(source, true) if source.vector(field, column).is_some_and(|source| source.source.is_some()));
        let source = |document: DocId, change: &Change| {
            if !change.present() {
                return (document, None);
            }
            let Change::Captured(source, _) = change else {
                unreachable!("validated private source");
            };
            (
                document,
                Some(
                    source
                        .vector(field, column)
                        .expect("validated private column")
                        .source
                        .as_ref()
                        .expect("validated private physical source")
                        .clone(),
                ),
            )
        };
        if self.staged_views().is_empty() && !self.has_identities() {
            for (_, change) in self.rows() {
                control.check()?;
                if change.present() && !physical(change) {
                    return Ok(None);
                }
            }
            return DiskANNReadChanges::capture(
                self.rows()
                    .iter()
                    .map(|(document, change)| Ok(source(*document, change))),
                control,
            )
            .map(Some);
        }
        // Rows that commands staged are evaluated fields, which no physical source supplies, so only their deletions leave a physical selection; the rows a transaction changed keep the sources captured with them.
        for change in self.change_rows_after(None) {
            control.check()?;
            let (_, change) = change?;
            if change.present() && !physical(&change) {
                return Ok(None);
            }
        }
        DiskANNReadChanges::capture(
            self.change_rows_after(None)
                .map(|change| change.map(|(document, change)| source(document, &change))),
            control,
        )
        .map(Some)
    }

    pub(in crate::query) fn vector_read_selection(
        &self,
        base: Option<VectorReadSnapshot>,
        dimensions: u32,
        field: &str,
        column: Option<&ColumnDef>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<VectorReadSnapshot>> {
        let values = |change: &Change| match change {
            Change::Captured(source, true) => source
                .vector(field, column)
                .map(|source| source.values.clone()),
            _ => None,
        };
        if self.staged_views().is_empty() && !self.has_identities() {
            for (_, change) in self.rows() {
                control.check()?;
                if change.present() && values(change).is_none() {
                    return Ok(None);
                }
            }
            return SelectedVectorRead::capture(
                base,
                dimensions,
                self.rows()
                    .iter()
                    .map(|(document, change)| (*document, values(change))),
                control,
            )
            .map(Some);
        }
        // Rows that commands staged are evaluated fields, which no physical source supplies, so only their deletions leave a physical selection; the rows a transaction changed keep the sources captured with them.
        let mut selected = BudgetedVec::new(control.memory());
        for change in self.change_rows_after(None) {
            control.check()?;
            let (document, change) = change?;
            let source = values(&change);
            if change.present() && source.is_none() {
                return Ok(None);
            }
            selected.push((document, source))?;
        }
        let (selected, _memory) = selected.into_parts();
        SelectedVectorRead::capture(base, dimensions, selected, control).map(Some)
    }
}
