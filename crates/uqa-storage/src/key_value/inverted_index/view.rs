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
        self.mutate(|view, batch| view.add_documents(batch, documents))
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

    pub(super) fn retained_snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        let source = match &self.source {
            OccurrenceSource::Retained(_) => self.source.clone(),
            OccurrenceSource::Live(_) => self.read(|view| {
                let mut prefixes = super::format::legacy_prefixes(view.table)?;
                prefixes.push(super::keys::table_prefix(view.table)?);
                view.store
                    .retain(&prefixes.iter().map(Vec::as_slice).collect::<Vec<_>>())
                    .map(OccurrenceSource::Retained)
            })?,
        };
        Ok(Arc::new(Self {
            source,
            table: self.table.clone(),
            bindings: self.bindings.clone(),
        }))
    }
}
