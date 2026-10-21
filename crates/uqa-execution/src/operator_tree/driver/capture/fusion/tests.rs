//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{CancellationToken, ScoredEntry};

fn input(prior: f64, score: f64) -> (CapturedScoredEntries, Option<f64>) {
    let mut rows = ScoredEntryCapture::new(0, CancellationToken::new()).unwrap();
    rows.push(ScoredEntry { doc_id: 7, score }).unwrap();
    (rows.finish().unwrap(), Some(prior))
}

#[test]
fn captured_exact_evidence_resolves_one_prior_and_preserves_score_bits() {
    for explicit in [None, Some(0.7)] {
        let mut output = fuse_captured(
            vec![input(0.2, 0.8), input(0.2, 0.3)],
            explicit,
            0,
            CancellationToken::new(),
            || Ok(()),
        )
        .unwrap();
        let actual = output.next().unwrap().unwrap();
        let expected = BayesianEvidenceFusion::new(explicit.unwrap_or(0.2))
            .unwrap()
            .fuse(&[
                uqa_scoring::EvidenceLogit::from_prior_free_probability(0.8).unwrap(),
                uqa_scoring::EvidenceLogit::from_prior_free_probability(0.3).unwrap(),
            ])
            .unwrap()
            .value();
        assert_eq!(actual.doc_id, 7);
        assert_eq!(actual.score.to_bits(), expected.to_bits());
        assert!(output.next().is_none());
    }
}

#[test]
fn captured_exact_evidence_keeps_prior_snapshot_and_probability_error_order() {
    let error = fuse_captured(
        vec![input(0.2, f64::NAN), input(0.3, 0.8)],
        None,
        0,
        CancellationToken::new(),
        || -> DriverResult<()> { panic!("conflicting priors precede the snapshot") },
    )
    .err()
    .unwrap();
    assert!(
        matches!(error, SQLError::TypeMismatch(message) if message.contains("one corpus prior"))
    );
    let error = fuse_captured(
        vec![input(0.2, f64::NAN)],
        Some(0.0),
        0,
        CancellationToken::new(),
        || -> DriverResult<()> { Err(SQLError::UnknownTable("snapshot".into())) },
    )
    .err()
    .unwrap();
    assert!(matches!(error, SQLError::UnknownTable(name) if name == "snapshot"));
    let error = fuse_captured(
        vec![input(0.2, f64::NAN)],
        Some(0.0),
        0,
        CancellationToken::new(),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("base_rate"));
    let error = fuse_captured(
        vec![input(0.2, f64::NAN)],
        Some(0.5),
        0,
        CancellationToken::new(),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("probability"));
    assert!(
        matches!(error, SQLError::Internal(message) if message.starts_with("execute BayesianEvidenceFusion:"))
    );
}

#[test]
fn captured_exact_evidence_keeps_cancellation_typed() {
    let cancellation = CancellationToken::new();
    let error = fuse_captured(vec![input(0.2, 0.8)], None, 0, cancellation.clone(), || {
        cancellation.cancel();
        Ok(())
    })
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
}
