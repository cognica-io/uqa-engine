//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{CancellationToken, ScoredEntry};
use uqa_fusion::{LogitGating, RobustPositiveEvidencePool};
use uqa_sql::SQLError;

fn options() -> PositiveEvidencePoolOptions<'static> {
    PositiveEvidencePoolOptions {
        alpha: 0.5,
        gating: LogitGating::Pass,
        base_rate: None,
        weights: None,
        adaptive_weights: false,
        logit_min: None,
        logit_max: None,
    }
}

fn input(prior: f64, score: Option<f64>) -> (CapturedScoredEntries, Option<f64>) {
    let mut capture = ScoredEntryCapture::new(0, CancellationToken::new()).unwrap();
    if let Some(score) = score {
        capture.push(ScoredEntry { doc_id: 9, score }).unwrap();
    }
    (capture.finish().unwrap(), Some(prior))
}

#[test]
fn captured_positive_pool_keeps_priors_empty_signals_and_score_bits() {
    let prior =
        uqa_scoring::sigmoid([0.2, 0.4, 0.3].map(uqa_scoring::logit).iter().sum::<f64>() / 3.0);
    let expected = RobustPositiveEvidencePool::new(0.5)
        .unwrap()
        .with_logit_gating(LogitGating::Pass)
        .with_base_rate(prior)
        .unwrap()
        .fuse_sparse(&[Some(0.8), None, Some(0.3)]);
    let mut actual = pool_captured(
        vec![
            input(0.2, Some(0.8)),
            input(0.4, None),
            input(0.3, Some(0.3)),
        ],
        options(),
        0,
        CancellationToken::new(),
        || Ok(()),
    )
    .unwrap();
    let entry = actual.next().unwrap().unwrap();
    assert_eq!(entry.doc_id, 9);
    assert_eq!(entry.score.to_bits(), expected.to_bits());
    assert!(actual.next().is_none());
}

#[test]
fn captured_positive_pool_keeps_snapshot_configuration_probability_error_order() {
    let invalid = PositiveEvidencePoolOptions {
        alpha: f64::NAN,
        ..options()
    };
    let error = pool_captured(
        vec![input(0.2, Some(f64::NAN))],
        invalid,
        0,
        CancellationToken::new(),
        || -> DriverResult<()> { Err(SQLError::UnknownTable("snapshot".into())) },
    )
    .err()
    .unwrap();
    assert!(matches!(error, SQLError::UnknownTable(name) if name == "snapshot"));
    let error = pool_captured(
        vec![input(0.2, Some(f64::NAN))],
        invalid,
        0,
        CancellationToken::new(),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("alpha"));
    let error = pool_captured(
        vec![input(0.2, Some(f64::NAN))],
        options(),
        0,
        CancellationToken::new(),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("probability"));
    assert!(
        matches!(error, SQLError::Internal(message) if message.starts_with("execute RobustPositiveEvidencePool:"))
    );
}

#[test]
fn captured_positive_pool_keeps_cancellation_typed() {
    let cancellation = CancellationToken::new();
    let error = pool_captured(
        vec![input(0.2, Some(0.8))],
        options(),
        0,
        cancellation.clone(),
        || {
            cancellation.cancel();
            Ok(())
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
}
