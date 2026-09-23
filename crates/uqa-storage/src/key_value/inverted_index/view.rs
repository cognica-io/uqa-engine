//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One reader owns an occurrence operation; retained indexes own their selected bytes.

use super::{AnalyzerBindings, Arc, DocId, FieldName, InvertedIndex, KeyValueInvertedIndex};
use crate::key_value::{index_view, KeyValueBatch, KeyValueRead, OccurrenceStorage};
use crate::StorageBackendResult;
use uqa_core::memory::BudgetedVec;

type Entry = (BudgetedVec<u8>, BudgetedVec<u8>);

#[derive(Clone)]
pub(super) enum OccurrenceSource {
    Live(Arc<dyn OccurrenceStorage>),
    Retained(Arc<dyn KeyValueRead + Send + Sync>),
}

pub(super) struct OccurrenceRead<'a> {
    pub(super) store: &'a dyn KeyValueRead,
    pub(super) table: &'a str,
    pub(super) bindings: &'a AnalyzerBindings,
}

impl OccurrenceRead<'_> {
    pub(super) fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<BudgetedVec<Entry>> {
        let mut rows = BudgetedVec::new(self.store.control().memory());
        append_rows(self.store, prefix, &mut rows)?;
        Ok(rows)
    }
}

fn append_rows(
    read: &dyn KeyValueRead,
    prefix: &[u8],
    rows: &mut BudgetedVec<Entry>,
) -> StorageBackendResult<()> {
    read.visit_prefix(prefix, &mut |key, value| {
        read.control().check()?;
        rows.reserve(1)?;
        let mut owned_key = BudgetedVec::new(read.control().memory());
        owned_key.extend_from_slice(key)?;
        let mut owned_value = BudgetedVec::new(read.control().memory());
        owned_value.extend_from_slice(value)?;
        rows.push((owned_key, owned_value))?;
        Ok(())
    })
}

impl KeyValueInvertedIndex {
    pub(super) fn read<T>(
        &self,
        operation: impl FnOnce(OccurrenceRead<'_>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let evaluate = |store: &dyn KeyValueRead| {
            operation(OccurrenceRead {
                store,
                table: &self.table,
                bindings: &self.bindings,
            })
        };
        match &self.source {
            OccurrenceSource::Live(store) => {
                index_view::read_scope(|operation| store.read(operation), evaluate)
            }
            OccurrenceSource::Retained(read) => {
                read.control().check()?;
                evaluate(read.as_ref())
            }
        }
    }

    pub(super) fn ensure_writable(&self) -> StorageBackendResult<()> {
        match self.source {
            OccurrenceSource::Live(_) => Ok(()),
            OccurrenceSource::Retained(_) => {
                Err(super::other_error("inverted-index snapshots are read-only"))
            }
        }
    }

    pub(super) fn mutate(
        &self,
        operation: impl FnOnce(&OccurrenceRead<'_>, &mut dyn KeyValueBatch) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.ensure_writable()?;
        let OccurrenceSource::Live(store) = &self.source else {
            unreachable!()
        };
        index_view::mutation_scope(
            |operation| store.mutate(operation),
            |read, batch| {
                operation(
                    &OccurrenceRead {
                        store: read,
                        table: &self.table,
                        bindings: &self.bindings,
                    },
                    batch,
                )
            },
        )
    }

    pub(super) fn add_documents(
        &self,
        documents: Vec<(DocId, std::collections::BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.mutate(|view, batch| view.add_documents(batch, documents, None))
    }

    pub(super) fn rebuild_documents(
        &self,
        documents: Vec<(DocId, std::collections::BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.rebuild_documents_inner(documents, None)
    }

    pub(super) fn rebuild_documents_inner(
        &self,
        documents: Vec<(DocId, std::collections::BTreeMap<FieldName, String>)>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> StorageBackendResult<()> {
        self.mutate(|view, batch| view.rebuild_documents_inner(batch, documents, cancellation))
    }

    /// Capture a provider's selected occurrence view without first cloning its analyzer bindings or table metadata into an intermediate live index.
    pub fn snapshot_from_read(
        read: &dyn KeyValueRead,
        table: &str,
        bindings: &AnalyzerBindings,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        read.control().check()?;
        let prefixes = super::format::retained_prefixes(table, read.control())?;
        let borrowed: [&[u8]; 8] = std::array::from_fn(|slot| &prefixes[slot][..]);
        let source = read.retain(&borrowed)?;
        Self::snapshot_from_source(source, table, bindings)
    }

    pub(super) fn retained_snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        match &self.source {
            OccurrenceSource::Retained(source) => {
                Self::snapshot_from_source(Arc::clone(source), &self.table, &self.bindings)
            }
            OccurrenceSource::Live(_) => {
                self.read(|view| Self::snapshot_from_read(view.store, view.table, view.bindings))
            }
        }
    }

    fn snapshot_from_source(
        source: Arc<dyn KeyValueRead + Send + Sync>,
        table: &str,
        bindings: &AnalyzerBindings,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        use crate::ReadOnlySnapshot;
        use uqa_core::memory::BudgetedString;

        let control = source.control().clone();
        control.check()?;
        let bindings = bindings.retained(&control)?;
        let mut name = BudgetedString::new(control.memory());
        name.reserve(table.len())?;
        for (offset, character) in table.chars().enumerate() {
            if offset % 1024 == 0 {
                control.check()?;
            }
            name.push(character)?;
        }
        let (table, mut memory) = name.into_parts();
        memory.grow(size_of::<Self>())?;
        memory.grow(size_of::<ReadOnlySnapshot<dyn InvertedIndex>>())?;
        let snapshot: Arc<dyn InvertedIndex> = Arc::new(Self {
            source: OccurrenceSource::Retained(source),
            table,
            bindings,
        });
        let snapshot = ReadOnlySnapshot::with_retention(snapshot, memory)?
            .with_inverted_read_control(&control)?;
        Ok(Arc::new(snapshot))
    }
}
