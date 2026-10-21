//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optimizing an evidence tree must preserve its arithmetic and failing-signal observations.

use super::QueryOptimizer;
use std::sync::Arc;
use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_operators::{
    BayesianEvidenceFusionOperator, ExecutionContext, GatingSpec, HybridProbBoolMode, Operator,
    OperatorTree, ProbBoolFusionOperator, RobustPositiveEvidencePoolOperator,
};
use uqa_storage::StorageBackendError;

struct Probability(String);
impl Operator for Probability {
    fn execute(&self, _: &ExecutionContext) -> uqa_operators::base::OperatorResult {
        let probability = self
            .0
            .parse::<f64>()
            .map_err(|_| StorageBackendError::Other(self.0.clone()))?;
        Ok(PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            7,
            Payload::with_score(probability),
        )]))
    }
}

fn leaf(probability: &str, k: usize) -> OperatorTree {
    OperatorTree::CalibratedVectorMatch {
        query_vector: vec![1.0, 0.0],
        k,
        field: probability.into(),
        threshold: None,
    }
}

fn fusion(signals: Vec<OperatorTree>, family: usize) -> OperatorTree {
    match family {
        0 => OperatorTree::BayesianEvidenceFusion {
            signals,
            base_rate: Some(0.2),
        },
        1 => OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha: 0.5,
            gating: GatingSpec::Softplus,
            weights: Some(vec![0.5, 0.3, 0.2]),
            logit_min: Some(vec![-3.0, -2.0, -1.0]),
            logit_max: Some(vec![4.0, 3.0, 2.0]),
            adaptive_weights: false,
        },
        2 => OperatorTree::ProbBoolFusion {
            signals,
            mode: uqa_operators::tree::ProbBoolMode::And,
        },
        _ => OperatorTree::ProbBoolFusion {
            signals,
            mode: uqa_operators::tree::ProbBoolMode::Or,
        },
    }
}

fn bind(tree: &OperatorTree) -> Arc<dyn Operator> {
    match tree {
        OperatorTree::CalibratedVectorMatch { field, .. } => Arc::new(Probability(field.clone())),
        OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
            Arc::new(BayesianEvidenceFusionOperator::new(
                signals.iter().map(bind).collect(),
                base_rate.unwrap(),
            ))
        }
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha,
            weights,
            logit_min,
            logit_max,
            ..
        } => Arc::new(
            RobustPositiveEvidencePoolOperator::new(signals.iter().map(bind).collect(), *alpha)
                .with_weights(weights.clone().unwrap())
                .with_logit_normalization(logit_min.clone().unwrap(), logit_max.clone().unwrap()),
        ),
        OperatorTree::ProbBoolFusion { signals, mode } => Arc::new(ProbBoolFusionOperator::new(
            signals.iter().map(bind).collect(),
            match mode {
                uqa_operators::tree::ProbBoolMode::And => HybridProbBoolMode::And,
                uqa_operators::tree::ProbBoolMode::Or => HybridProbBoolMode::Or,
            },
        )),
        _ => panic!("unexpected fusion fixture"),
    }
}

fn signal_fields(tree: &OperatorTree) -> Vec<String> {
    let mut fields = Vec::new();
    tree.visit(&mut |node| {
        if let OperatorTree::CalibratedVectorMatch { field, .. } = node {
            fields.push(field.clone());
        }
    });
    fields
}

#[allow(
    deprecated,
    reason = "verify that the legacy public flag cannot restore an unsafe rewrite"
)]
fn optimizer(flag: bool) -> QueryOptimizer {
    let mut optimizer = if flag {
        QueryOptimizer::new().with_graph_stats(crate::cardinality::GraphStats {
            num_vertices: 1000,
            num_edges: 2000,
            ..Default::default()
        })
    } else {
        QueryOptimizer::new()
    };
    optimizer.config.enable_reorder_fusion_signals = flag;
    optimizer
}

#[test]
fn fusion_costs_preserve_declared_score_bits_and_positional_parameters() {
    let context = ExecutionContext::new();
    for values in [
        ["0.7", "0.1", "0.6"],
        ["0.9999999999999", "0.0000000000001", "0.7"],
        ["0.5", "0.75", "0.25"],
    ] {
        for family in 0..4 {
            for flag in [false, true] {
                let source = fusion(
                    values
                        .into_iter()
                        .zip([100, 2, 1])
                        .map(|(value, k)| leaf(value, k))
                        .collect(),
                    family,
                );
                let expected = bind(&source).execute(&context).unwrap();
                let optimized = optimizer(flag).optimize(source.clone());
                assert_eq!(signal_fields(&optimized), signal_fields(&source));
                let actual = bind(&optimized).execute(&context).unwrap();
                assert_eq!(
                    actual.entries()[0].payload.score.to_bits(),
                    expected.entries()[0].payload.score.to_bits(),
                    "family={family}, flag={flag}"
                );
                assert_eq!(actual.entries()[0].doc_id, 7);
            }
        }
    }
}

#[test]
fn nested_fusion_keeps_declared_failing_signal_precedence() {
    let context = ExecutionContext::new();
    for family in 0..4 {
        let nested = fusion(
            vec![
                leaf("first expensive error", 100),
                leaf("second cheap error", 1),
                leaf("0.5", 2),
            ],
            family,
        );
        let source = fusion(vec![nested, leaf("0.2", 1), leaf("0.8", 2)], 0);
        for flag in [false, true] {
            let actual = bind(&optimizer(flag).optimize(source.clone()))
                .execute(&context)
                .unwrap_err();
            assert_eq!(
                actual.to_string(),
                bind(&source).execute(&context).unwrap_err().to_string()
            );
            assert!(actual.to_string().contains("first expensive error"));
        }
    }
}
