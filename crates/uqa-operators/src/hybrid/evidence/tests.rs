//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn entry(doc_id: DocId, score: f64) -> ScoredEntry {
    ScoredEntry { doc_id, score }
}

#[test]
fn exact_evidence_cursor_preserves_sparse_union_duplicate_scores_and_float_order() {
    let signals = [
        vec![
            entry(0, -0.0),
            entry(3, 0.7),
            entry(3, 0.8),
            entry(u64::MAX, 0.5),
        ],
        vec![entry(1, 1.0), entry(3, 0.2)],
        vec![],
        vec![entry(3, 0.9), entry(u64::MAX, 0.01)],
    ];
    let maps = signals
        .iter()
        .map(|signal| {
            signal
                .iter()
                .map(|e| (e.doc_id, e.score))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let identities = maps
        .iter()
        .flat_map(|map| map.keys().copied())
        .collect::<BTreeSet<_>>();
    for prior in [0.01, 0.5, 0.99] {
        let fusion = BayesianEvidenceFusion::new(prior).unwrap();
        let expected = identities
            .iter()
            .map(|&id| {
                let evidence = maps
                    .iter()
                    .filter_map(|map| map.get(&id))
                    .map(|p| EvidenceLogit::from_prior_free_probability(*p).unwrap())
                    .collect::<Vec<_>>();
                (id, fusion.fuse(&evidence).unwrap().value().to_bits())
            })
            .collect::<Vec<_>>();
        let mut actual = Vec::new();
        visit_exact_evidence(
            &fusion,
            signals.len(),
            |i| Ok::<_, &'static str>(signals[i].iter().cloned().map(Ok)),
            |e| {
                actual.push((e.doc_id, e.score.to_bits()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn exact_evidence_validates_all_signals_before_any_output() {
    for invalid in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let signals = [vec![entry(1, 0.5)], vec![entry(2, 0.8), entry(3, invalid)]];
        let error = visit_exact_evidence(
            &BayesianEvidenceFusion::default(),
            signals.len(),
            |i| Ok::<_, &'static str>(signals[i].iter().cloned().map(Ok)),
            |_| panic!("invalid later signal must fail before output"),
        )
        .unwrap_err();
        assert!(
            matches!(error, EvidenceVisitError::Fusion(error) if error.to_string().contains("probability"))
        );
    }
}

#[test]
fn exact_evidence_preserves_input_and_consumer_failures() {
    let error = visit_exact_evidence(
        &BayesianEvidenceFusion::default(),
        1,
        |_| Ok([Ok(entry(1, 0.5)), Err("input failure")].into_iter()),
        |_| panic!("invalid input must precede output"),
    )
    .unwrap_err();
    assert!(matches!(error, EvidenceVisitError::Input("input failure")));
    let error = visit_exact_evidence(
        &BayesianEvidenceFusion::default(),
        1,
        |_| Ok([Ok(entry(1, 0.5)), Ok(entry(2, 0.6))].into_iter()),
        |_| Err("consumer failure"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        EvidenceVisitError::Output("consumer failure")
    ));
}

#[test]
fn exact_evidence_retains_only_live_signal_heads() {
    let mut visited = 0_u64;
    let allocated = allocation_counter::measure(|| {
        visit_exact_evidence(
            &BayesianEvidenceFusion::default(),
            3,
            |signal| {
                Ok::<_, &'static str>(
                    (0..32_768).map(move |id| Ok(entry(id, if signal == 1 { 0.2 } else { 0.8 }))),
                )
            },
            |e| {
                assert_eq!(e.doc_id, visited);
                assert!(e.score > 0.5);
                visited += 1;
                Ok(())
            },
        )
        .unwrap();
    });
    assert_eq!(visited, 32_768);
    assert!(allocated.bytes_max < 16 * 1024, "{allocated:?}");
}

#[test]
fn exact_evidence_empty_inputs_never_synthesize_documents() {
    for signals in [0, 3] {
        visit_exact_evidence(
            &BayesianEvidenceFusion::default(),
            signals,
            |_| Ok::<_, &'static str>(std::iter::empty()),
            |_| panic!("no signal has a document"),
        )
        .unwrap();
    }
}
