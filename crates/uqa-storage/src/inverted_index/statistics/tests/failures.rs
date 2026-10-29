//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{DocId, FieldName, IndexStats, PostingList};

struct FailingRead {
    inner: Box<dyn InvertedIndex>,
    calls: AtomicUsize,
    panic: bool,
}

impl InvertedIndex for FailingRead {
    fn analyzer(&self) -> &uqa_analysis::Analyzer {
        self.inner.analyzer()
    }
    fn add_document(
        &mut self,
        _: DocId,
        _: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn remove_document(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn get_posting_list(&self, _: &str, _: &str) -> StorageBackendResult<PostingList> {
        unreachable!()
    }
    fn doc_freq(&self, _: &str, _: &str) -> StorageBackendResult<u64> {
        unreachable!()
    }
    fn get_doc_length(&self, _: DocId, _: &str) -> StorageBackendResult<u64> {
        unreachable!()
    }
    fn get_term_freq(&self, _: DocId, _: &str, _: &str) -> StorageBackendResult<u64> {
        unreachable!()
    }
    fn doc_count(&self) -> StorageBackendResult<u64> {
        unreachable!()
    }
    fn total_field_length(&self, _: &str) -> StorageBackendResult<u64> {
        unreachable!()
    }
    fn stats(&self) -> StorageBackendResult<IndexStats> {
        unreachable!()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        unreachable!()
    }
    fn search_analyzer_revision(
        &self,
        field: &str,
    ) -> StorageBackendResult<Arc<uqa_analysis::CompiledAnalyzer>> {
        self.inner.search_analyzer_revision(field)
    }
    fn field_names_budgeted(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<BudgetedString>> {
        self.inner.field_names_budgeted(control)
    }
    fn doc_freq_key_budgeted(
        &self,
        _: &str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        assert!(
            control.memory().used() > 7,
            "the analyzer output and current key must stay owned during reads"
        );
        match self.calls.fetch_add(1, Ordering::Relaxed) {
            0 => {
                assert_eq!(term.as_str(), Some("a"));
                Ok(1)
            }
            1 if self.panic => panic!("provider unwind"),
            1 => Err(StorageBackendError::Other("provider failure".into())),
            _ => panic!("must stop at the first provider failure"),
        }
    }
}

#[test]
fn frequency_failure_and_unwind_retire_analysis_keys_and_field_inventory() {
    for panic in [false, true] {
        for field in [Some("long_field"), None] {
            let index = FailingRead {
                inner: index(false),
                calls: AtomicUsize::new(0),
                panic,
            };
            let control = StorageReadControl::with_limit(1 << 20);
            let sibling = control.memory().reserve(7).unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                query_document_frequency(&index, field, "a b unreached", &control)
            }));
            if panic {
                assert!(outcome.is_err());
            } else {
                assert!(
                    matches!(outcome.unwrap(), Err(StorageBackendError::Other(message)) if message == "provider failure")
                );
            }
            assert_eq!(index.calls.load(Ordering::Relaxed), 2);
            assert_eq!(control.memory().used(), 7);
            drop(sibling);
        }
    }
}

#[test]
fn retained_and_invoking_cancellation_remain_independent() {
    for original in [false, true] {
        let retained_control = StorageReadControl::with_limit(1 << 20);
        let invoking_control = StorageReadControl::with_limit(1 << 20);
        let retained = crate::read_only_snapshot::ReadOnlySnapshot::new(
            Arc::<dyn InvertedIndex>::from(index(false)),
        )
        .with_inverted_read_control(&retained_control)
        .unwrap();
        if original {
            retained_control.cancellation().cancel();
        } else {
            invoking_control.cancellation().cancel();
        }
        assert!(matches!(
            query_document_frequency(&retained, None, "a b", &invoking_control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(retained_control.memory().used(), 0);
        assert_eq!(invoking_control.memory().used(), 0);
    }
}
