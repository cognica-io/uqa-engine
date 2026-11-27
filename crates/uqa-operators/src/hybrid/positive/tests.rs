//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn options() -> PositiveEvidencePoolOptions<'static> {
    PositiveEvidencePoolOptions {
        alpha: 0.5,
        gating: LogitGating::Softplus,
        base_rate: None,
        weights: None,
        adaptive_weights: false,
        logit_min: None,
        logit_max: None,
    }
}

fn reference(
    inputs: &[Vec<ScoredEntry>],
    config: PositiveEvidencePoolOptions<'_>,
) -> Vec<(u64, u64)> {
    let maps = inputs
        .iter()
        .map(|rows| {
            rows.iter()
                .map(|entry| (entry.doc_id, entry.score))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let mut fuser = RobustPositiveEvidencePool::new(config.alpha)
        .unwrap()
        .with_logit_gating(config.gating);
    if let Some(prior) = config.base_rate {
        fuser = fuser.with_base_rate(prior).unwrap();
    }
    let adaptive = (config.weights.is_none() && config.adaptive_weights)
        .then(|| super::super::tests::adaptive_signal_weights(&fuser, &maps))
        .flatten();
    maps.iter()
        .flat_map(|map| map.keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            let probabilities = maps
                .iter()
                .map(|map| map.get(&id).copied())
                .collect::<Vec<_>>();
            let score = fuser
                .fuse_configured(
                    &probabilities,
                    config.weights.or(adaptive.as_deref()),
                    config.logit_min,
                    config.logit_max,
                )
                .unwrap();
            (id, score.to_bits())
        })
        .collect()
}

#[test]
fn positive_cursor_matches_original_sparse_duplicate_and_adaptive_score_bits() {
    let inputs = [
        [(0, 0.0), (0, -0.0), (1, 0.9), (8, 0.2), (u64::MAX, 1.0)]
            .into_iter()
            .map(|(doc_id, score)| ScoredEntry { doc_id, score })
            .collect::<Vec<_>>(),
        [(1, 0.2), (7, 0.9), (8, 0.4), (8, 0.8), (u64::MAX, 0.0)]
            .into_iter()
            .map(|(doc_id, score)| ScoredEntry { doc_id, score })
            .collect(),
        Vec::new(),
    ];
    for gating in [
        LogitGating::Softplus,
        LogitGating::Pass,
        LogitGating::Sigmoid,
        LogitGating::ReLU,
        LogitGating::Swish,
        LogitGating::Gelu,
    ] {
        for alpha in [0.0, 0.5, 1.0] {
            for base_rate in [None, Some(0.2)] {
                for adaptive_weights in [false, true] {
                    for weights in [None, Some(&[0.2, 0.3, 0.5][..])] {
                        for normalize in [false, true] {
                            let config = PositiveEvidencePoolOptions {
                                alpha,
                                gating,
                                base_rate,
                                weights,
                                adaptive_weights,
                                logit_min: normalize.then_some(&[-2.0, -3.0, -4.0]),
                                logit_max: normalize.then_some(&[2.0, 3.0, 4.0]),
                            };
                            let expected = reference(&inputs, config);
                            let pool =
                                PreparedPositiveEvidencePool::new(config, inputs.len()).unwrap();
                            let mut actual = Vec::new();
                            pool.visit(
                                |i| Ok::<_, ()>(inputs[i].iter().cloned().map(Ok)),
                                |entry| {
                                    actual.push((entry.doc_id, entry.score.to_bits()));
                                    Ok(())
                                },
                            )
                            .unwrap();
                            assert_eq!(actual, expected);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn adaptive_validation_and_mean_share_a_replay_and_preserve_centered_score_bits() {
    let samples = [[0.01, 0.01, 0.7], [0.2, 0.7, 0.9]];
    let inputs = samples.map(|signal| {
        signal
            .into_iter()
            .enumerate()
            .map(|(doc_id, score)| ScoredEntry {
                doc_id: doc_id as u64,
                score,
            })
            .collect::<Vec<_>>()
    });
    let config = PositiveEvidencePoolOptions {
        gating: LogitGating::Pass,
        adaptive_weights: true,
        ..options()
    };
    let expected = reference(&inputs, config);
    let mut replays = [0; 2];
    let mut actual = Vec::new();
    PreparedPositiveEvidencePool::new(config, 2)
        .unwrap()
        .visit(
            |i| {
                replays[i] += 1;
                Ok::<_, ()>(inputs[i].iter().cloned().map(Ok))
            },
            |entry| {
                actual.push((entry.doc_id, entry.score.to_bits()));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(replays, [3, 3]);
}

#[test]
fn adaptive_validation_rejects_an_invalid_duplicate_before_output() {
    let config = PositiveEvidencePoolOptions {
        adaptive_weights: true,
        ..options()
    };
    let pool = PreparedPositiveEvidencePool::new(config, 1).unwrap();
    let error = pool
        .visit(
            |_| {
                Ok::<_, ()>(
                    [f64::NAN, 0.4]
                        .into_iter()
                        .map(|score| Ok(ScoredEntry { doc_id: 12, score })),
                )
            },
            |_| panic!("invalid duplicate reached the consumer"),
        )
        .unwrap_err();
    assert!(
        matches!(error, EvidenceVisitError::Fusion(cause) if cause.to_string().contains("positive-evidence pool requires probability"))
    );
}

#[test]
fn positive_cursor_keeps_empty_signals_in_the_declared_count() {
    let inputs = [
        vec![ScoredEntry {
            doc_id: 7,
            score: 0.9,
        }],
        Vec::new(),
        Vec::new(),
    ];
    for inputs in [&inputs[..], &inputs[1..], &inputs[..1]] {
        let pool = PreparedPositiveEvidencePool::new(options(), inputs.len()).unwrap();
        let mut actual = Vec::new();
        pool.visit(
            |i| Ok::<_, ()>(inputs[i].iter().cloned().map(Ok)),
            |entry| {
                actual.push((entry.doc_id, entry.score.to_bits()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual, reference(inputs, options()));
    }
}

#[test]
fn positive_cursor_bounds_live_memory_for_complete_adaptive_inputs() {
    let config = PositiveEvidencePoolOptions {
        adaptive_weights: true,
        ..options()
    };
    let pool = PreparedPositiveEvidencePool::new(config, 3).unwrap();
    let mut count = 0;
    let allocated = allocation_counter::measure(|| {
        pool.visit(
            |signal| {
                Ok::<_, ()>((0..65_536u64).map(move |doc_id| {
                    Ok(ScoredEntry {
                        doc_id,
                        score: if (doc_id + signal as u64).is_multiple_of(3) {
                            0.2
                        } else {
                            0.9
                        },
                    })
                }))
            },
            |entry| {
                assert_eq!(entry.doc_id, count);
                assert!(entry.score.is_finite());
                count += 1;
                Ok(())
            },
        )
        .unwrap();
    });
    assert_eq!(count, 65_536);
    assert!(allocated.bytes_max < 16 * 1024, "{allocated:?}");
}

#[test]
fn positive_cursor_validates_all_scores_before_output_and_preserves_typed_failures() {
    let pool = PreparedPositiveEvidencePool::new(options(), 2).unwrap();
    let error = pool
        .visit(
            |i| {
                Ok::<_, &str>(
                    [Ok(ScoredEntry {
                        doc_id: 1,
                        score: if i == 0 { 0.8 } else { f64::NAN },
                    })]
                    .into_iter(),
                )
            },
            |_| panic!("all inputs must be validated"),
        )
        .unwrap_err();
    assert!(
        matches!(error, EvidenceVisitError::Fusion(error) if error.to_string().contains("positive-evidence pool"))
    );
    let error = pool
        .visit(
            |_| Ok([Err::<ScoredEntry, _>("input failed")].into_iter()),
            |_| panic!("input failure precedes output"),
        )
        .unwrap_err();
    assert!(matches!(error, EvidenceVisitError::Input("input failed")));
    let error = pool
        .visit(
            |_| {
                Ok([Ok(ScoredEntry {
                    doc_id: 1,
                    score: 0.8,
                })]
                .into_iter())
            },
            |_| Err("consumer failed"),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        EvidenceVisitError::Output("consumer failed")
    ));
}
