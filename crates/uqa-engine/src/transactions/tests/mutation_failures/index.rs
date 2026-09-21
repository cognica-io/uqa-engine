//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inject cancellation after a native private mutation and before execution registers its captured intents.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use uqa_analysis::Analyzer;
use uqa_core::{CancellationToken, DocId, FieldName, IndexStats, PostingList};
use uqa_storage::{
    inverted_index::InvertedIndexChangeVisitor, InvertedIndex, StorageBackendResult,
};

pub(super) struct CancelAfterMutation {
    pub(super) index: Box<dyn InvertedIndex>,
    pub(super) cancellation: CancellationToken,
    pub(super) fired: Arc<AtomicBool>,
}

impl CancelAfterMutation {
    fn cancel(&self) {
        self.fired.store(true, Ordering::Release);
        self.cancellation.cancel();
    }
}

impl InvertedIndex for CancelAfterMutation {
    fn analyzer(&self) -> &Analyzer {
        self.index.analyzer()
    }
    fn add_document(
        &mut self,
        doc: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        self.index.add_document(doc, fields)?;
        self.cancel();
        Ok(())
    }
    fn try_add_documents_observed(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        visit: &mut InvertedIndexChangeVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.index.try_add_documents_observed(documents, visit)?;
        self.cancel();
        Ok(())
    }
    fn remove_document(&mut self, doc: DocId) -> StorageBackendResult<()> {
        self.index.remove_document(doc)?;
        self.cancel();
        Ok(())
    }
    fn try_remove_document_observed(
        &mut self,
        doc: DocId,
        visit: &mut InvertedIndexChangeVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.index.try_remove_document_observed(doc, visit)?;
        self.cancel();
        Ok(())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.index.clear()
    }
    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.index.get_posting_list(field, term)
    }
    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.index.doc_freq(field, term)
    }
    fn get_doc_length(&self, doc: DocId, field: &str) -> StorageBackendResult<u64> {
        self.index.get_doc_length(doc, field)
    }
    fn get_term_freq(&self, doc: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.index.get_term_freq(doc, field, term)
    }
    fn doc_count(&self) -> StorageBackendResult<u64> {
        self.index.doc_count()
    }
    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        self.index.total_field_length(field)
    }
    fn stats(&self) -> StorageBackendResult<IndexStats> {
        self.index.stats()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        self.index.snapshot()
    }
}
