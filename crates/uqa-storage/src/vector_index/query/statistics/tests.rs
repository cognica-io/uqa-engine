//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{read_control::StorageReadControl, StorageBackendResult, VectorIndex};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

#[derive(Default)]
struct Provider {
    ordinary: AtomicUsize,
    controlled: AtomicUsize,
    cancel: Option<uqa_core::CancellationToken>,
}

impl Provider {
    fn complete(&self) -> PostingList {
        if let Some(cancellation) = &self.cancel {
            cancellation.cancel();
        }
        Self::result()
    }
    fn result() -> PostingList {
        PostingList::from_sorted_unchecked(vec![PostingEntry::new(7, Payload::with_score(0.5))])
    }
}

impl VectorIndex for Provider {
    fn dimensions(&self) -> u32 {
        2
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn count(&self) -> StorageBackendResult<usize> {
        unreachable!()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        unreachable!()
    }
    fn search_knn(&self, _: &[f32], _: usize) -> StorageBackendResult<PostingList> {
        self.ordinary.fetch_add(1, Ordering::Relaxed);
        Ok(self.complete())
    }
    fn search_threshold(&self, _: &[f32], _: f32) -> StorageBackendResult<PostingList> {
        self.ordinary.fetch_add(1, Ordering::Relaxed);
        Ok(self.complete())
    }
    fn search_knn_with_control(
        &self,
        _: &[f32],
        _: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        assert_eq!(control.memory().limit(), 73);
        self.controlled.fetch_add(1, Ordering::Relaxed);
        Ok(self.complete())
    }
    fn search_threshold_with_control(
        &self,
        _: &[f32],
        _: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        assert_eq!(control.memory().limit(), 73);
        self.controlled.fetch_add(1, Ordering::Relaxed);
        Ok(self.complete())
    }
}

#[test]
fn vector_reports_default_to_unmeasured_after_exactly_one_original_dispatch() {
    let index = Provider::default();
    let control = StorageReadControl::with_limit(73);
    for invoking in [None, Some(&control)] {
        let knn = index
            .search_knn_with_statistics(&[1.0, 0.0], 1, invoking)
            .unwrap();
        assert_eq!(knn.postings, Provider::result());
        assert!(knn.diskann.is_none());
        let threshold = index
            .search_threshold_with_statistics(&[1.0, 0.0], 0.5, invoking)
            .unwrap();
        assert_eq!(threshold.postings, Provider::result());
        assert!(threshold.diskann.is_none());
    }
    assert_eq!(index.ordinary.load(Ordering::Relaxed), 2);
    assert_eq!(index.controlled.load(Ordering::Relaxed), 2);
    control.cancellation().cancel();
    assert!(index
        .search_knn_with_statistics(&[1.0, 0.0], 1, Some(&control))
        .is_err());
    assert!(index
        .search_threshold_with_statistics(&[1.0, 0.0], 0.5, Some(&control))
        .is_err());
    assert_eq!(index.controlled.load(Ordering::Relaxed), 2);
}

#[test]
fn vector_reports_reject_invoking_cancellation_during_a_bound_provider_call() {
    let invoking = StorageReadControl::with_limit(0);
    let provider = Arc::new(Provider {
        cancel: Some(invoking.cancellation().clone()),
        ..Provider::default()
    });
    let bound = StorageReadControl::with_limit(73);
    let reader = crate::ReadOnlySnapshot::new(provider.clone())
        .with_vector_read_control(&bound)
        .unwrap();
    assert!(matches!(
        reader.search_knn_with_statistics(&[1.0, 0.0], 1, Some(&invoking)),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(provider.controlled.load(Ordering::Relaxed), 1);
    invoking.cancellation().reset();
    assert!(matches!(
        reader.search_threshold_with_statistics(&[1.0, 0.0], 0.5, Some(&invoking)),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(provider.controlled.load(Ordering::Relaxed), 2);
    assert_eq!(provider.ordinary.load(Ordering::Relaxed), 0);
}
