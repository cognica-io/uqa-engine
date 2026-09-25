//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct InvalidSource(u8);
impl DiskANNCanonicalRead for InvalidSource {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        Ok((after.is_none() || self.0 == 4).then_some(1))
    }
    fn origin(
        &self,
        _: DocId,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        Ok(Some(version(1)))
    }
    fn visit_document(
        &self,
        _: DocId,
        _: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        match self.0 {
            0 => {
                let _ignored = visit(0, version(1), &[1.0]);
            }
            1 => {
                visit(1, version(1), &[1.0, 0.0])?;
            }
            2 => {
                visit(0, version(1), &[1.0, 0.0])?;
                visit(1, version(2), &[1.0, 0.0])?;
            }
            3 => {
                visit(0, version(1), &[1.0, 0.0])?;
                return Ok(Some(version(2)));
            }
            4 => {
                visit(0, version(1), &[1.0, 0.0])?;
            }
            5 => return Ok(None),
            _ => {
                visit(0, version(1), &[f32::NAN, 0.0])?;
            }
        }
        Ok(Some(version(1)))
    }
}

#[test]
fn diskann_canonical_scoring_rejects_broken_provider_contracts_and_suppressed_errors() {
    for fault in 0..7 {
        let source = InvalidSource(fault);
        let control = StorageReadControl::with_limit(8192);
        let scorer = DiskANNCanonicalScorer::new(&source, &[1.0, 0.0], &control).unwrap();
        let error = scorer.search_exact_knn(4).unwrap_err();
        if fault == 0 {
            assert!(error.to_string().contains("dimension mismatch"));
        }
        assert!(scorer.search_threshold(0.0).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn diskann_canonical_scoring_validates_queries_and_keeps_original_and_invoking_controls() {
    let source = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let control = StorageReadControl::with_limit(4096);
    for query in [
        vec![],
        vec![1.0],
        vec![f32::NAN, 0.0],
        vec![0.0, f32::INFINITY],
    ] {
        assert!(DiskANNCanonicalScorer::new(&source, &query, &control).is_err());
    }
    let scorer = DiskANNCanonicalScorer::new(&source, &[1.0, 0.0], &control).unwrap();
    for threshold in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(scorer.search_threshold(threshold).is_err());
    }
    let mut visits = 0;
    assert!(scorer
        .visit_scores(&mut |_| {
            visits += 1;
            Err(invalid("consumer refused score"))
        })
        .is_err());
    assert_eq!(visits, 1);
    source.control.cancellation().cancel();
    assert!(scorer.search_exact_knn(0).is_err());
    assert!(scorer.score_candidate(1, 0, version(2)).is_err());
    let source = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let scorer = DiskANNCanonicalScorer::new(&source, &[1.0, 0.0], &control).unwrap();
    control.cancellation().cancel();
    assert!(scorer.search_exact_knn(0).is_err());
    assert!(scorer.search_threshold(0.0).is_err());
    assert_eq!(control.memory().used(), 0);
}
