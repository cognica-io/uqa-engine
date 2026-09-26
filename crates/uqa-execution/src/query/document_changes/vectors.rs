//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private rows retain the physical canonical sources captured with their evaluated versions.

use super::{
    Arc, Change, DocumentChanges, DocumentSelection, DocumentStore, StorageBackendResult,
    StorageReadControl,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{
    diskann_index::{DiskANNReadChanges, DiskANNReadSnapshot},
    vector_index::{RetainedVectorIndexesBuilder, VectorIndexSource},
};

struct VectorSource {
    field: String,
    column: Option<[u8; 16]>,
    source: DiskANNReadSnapshot,
    _memory: MemoryReservation,
}

pub(super) struct CapturedRows {
    pub(super) documents: Arc<dyn DocumentStore>,
    vectors: BudgetedVec<VectorSource>,
}

impl CapturedRows {
    fn vector(&self, field: &str, column: Option<&ColumnDef>) -> Option<&DiskANNReadSnapshot> {
        let identity = column.and_then(|column| column.object_id);
        self.vectors
            .iter()
            .find(|source| match (source.column, identity) {
                (Some(source), Some(target)) => source == target,
                _ => source.field == field,
            })
            .map(|source| &source.source)
    }
}

impl DocumentChanges {
    /// Capture immutable rows and their actual canonical vector sources at the same selected table boundary. Each retained row, including a tombstone, keeps the source that supplied that version across later captures, schema renames and transaction completion. Other vector methods acquire no additional retained source.
    pub fn with_retained_vectors(
        mut self,
        documents: Arc<dyn DocumentStore>,
        desired: DocumentSelection,
        columns: &[ColumnDef],
        indexes: &dyn VectorIndexSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut vectors = BudgetedVec::new(control.memory());
        indexes.visit(&mut |field, index| {
            let Some(source) = index.diskann_read_snapshot(control)? else {
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
                _memory: memory,
            })?;
            Ok(())
        })?;
        if vectors.is_empty() {
            return self.with_retained(documents, desired, control);
        }
        let source = Budgeted::new(
            CapturedRows { documents, vectors },
            control.memory().empty_reservation(),
        )
        .into_shared()?;
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

    /// Select the actual source retained with each evaluated private row. None means this row source predates physical capture or has no matching column incarnation; callers must not substitute a later live source.
    pub(in crate::query) fn diskann_read_changes(
        &self,
        field: &str,
        column: Option<&ColumnDef>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNReadChanges>> {
        for (_, change) in self.rows() {
            control.check()?;
            if change.present()
                && !matches!(change, Change::Captured(source, true) if source.vector(field, column).is_some())
            {
                return Ok(None);
            }
        }
        DiskANNReadChanges::capture(
            self.rows().iter().map(|(document, change)| {
                if !change.present() {
                    return Ok((*document, None));
                }
                let Change::Captured(source, _) = change else {
                    unreachable!("validated private source");
                };
                Ok((
                    *document,
                    Some(
                        source
                            .vector(field, column)
                            .expect("validated private column")
                            .clone(),
                    ),
                ))
            }),
            control,
        )
        .map(Some)
    }
}
