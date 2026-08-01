//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Payload;
use uqa_operators::GraphNeighborLookup;

struct ConstOperator(Vec<PostingEntry>);

impl Operator for ConstOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
        Ok(PostingList::from_sorted_unchecked(self.0.clone()))
    }
}

fn entry(id: u64, score: f64) -> PostingEntry {
    PostingEntry::new(id, Payload::with_score(score))
}

/// Tiny adjacency-list neighbor lookup for the graph layer tests.
struct StaticGraph {
    out_edges: BTreeMap<u64, Vec<(String, u64)>>,
}

impl StaticGraph {
    fn new() -> Self {
        Self {
            out_edges: BTreeMap::new(),
        }
    }

    fn add(&mut self, src: u64, label: &str, dst: u64) {
        self.out_edges
            .entry(src)
            .or_default()
            .push((label.to_string(), dst));
    }
}

impl GraphNeighborLookup for StaticGraph {
    fn neighbors(
        &self,
        vertex: u64,
        label: &str,
        direction: Direction,
    ) -> StorageBackendResult<Vec<u64>> {
        let mut out = Vec::new();
        if matches!(direction, Direction::Out | Direction::Both) {
            if let Some(es) = self.out_edges.get(&vertex) {
                for (l, d) in es {
                    if label.is_empty() || l == label {
                        out.push(*d);
                    }
                }
            }
        }
        if matches!(direction, Direction::In | Direction::Both) {
            for (src, es) in &self.out_edges {
                for (l, d) in es {
                    if (label.is_empty() || l == label) && *d == vertex {
                        out.push(*src);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
}

#[test]
fn signal_only_pipeline_returns_sigmoid_of_logit() {
    let signal = Arc::new(ConstOperator(vec![entry(1, 0.8), entry(2, 0.2)])) as Arc<dyn Operator>;
    let op = DeepFusionOperator::new(vec![Layer::Signal(vec![signal])], 0.0, Gating::None)
        .expect("valid signal model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("signal-only deep fusion should execute");
    let scores: BTreeMap<u64, f64> = result
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    // Logit of 0.8 -> sigmoid back to ~0.8.
    assert!((scores[&1] - 0.8).abs() < 1e-6);
    assert!((scores[&2] - 0.2).abs() < 1e-6);
}

#[test]
fn embed_then_dense_then_softmax_classifies() {
    // Two-class linear classifier: x = [3, 1], W picks class 0.
    let layers = vec![
        Layer::Embed(vec![3.0, 1.0]),
        Layer::Flatten,
        Layer::Dense {
            weights: vec![1.0, 0.0, 0.0, 1.0],
            bias: vec![0.0, 0.0],
            output_channels: 2,
            input_channels: 2,
        },
        Layer::Softmax,
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid dense model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("dense deep fusion should execute");
    assert_eq!(result.entries().len(), 1);
    let payload = &result.entries()[0].payload;
    let probs = match payload.fields.get("class_probs") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Float(f) => Some(*f),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => panic!("missing class_probs"),
    };
    assert_eq!(probs.len(), 2);
    assert!(probs[0] > probs[1], "expected class 0 to win: {probs:?}");
    assert!((probs.iter().sum::<f64>() - 1.0).abs() < 1e-9);
}

#[test]
fn dropout_scales_inference_values() {
    let layers = vec![
        Layer::Embed(vec![2.0, 4.0]),
        Layer::Dropout { p: 0.5 },
        Layer::Flatten,
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid dropout model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("dropout deep fusion should execute");
    let entry = &result.entries()[0];
    // Final layer is Flatten so num_channels=2 and result reports
    // max-sigmoid on the post-dropout values [1.0, 2.0].
    let expected = [1.0_f64, 2.0_f64]
        .into_iter()
        .map(sigmoid)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((entry.payload.score - expected).abs() < 1e-9);
}

#[test]
fn cnn_1d_detects_adjacent_pattern() {
    let layers = vec![
        Layer::Embed(vec![1.0, 2.0, 3.0]),
        Layer::CNN1D {
            weights: vec![1.0, 1.0],
            bias: vec![0.0],
            output_channels: 1,
            input_channels: 1,
            kernel_size: 2,
            stride: 1,
            padding: 0,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid CNN1D model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("1-D convolution should execute");
    assert_eq!(result.entries().len(), 2);
    let scores: BTreeMap<u64, f64> = result
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    assert!((scores[&1] - sigmoid(3.0)).abs() < 1e-9);
    assert!((scores[&2] - sigmoid(5.0)).abs() < 1e-9);
}

#[test]
fn cnn_2d_filters_flattened_image() {
    let layers = vec![
        Layer::Embed(vec![1.0, 2.0, 3.0, 4.0]),
        Layer::Flatten,
        Layer::CNN2D {
            weights: vec![1.0, 1.0, 1.0, 1.0],
            bias: vec![0.0],
            output_channels: 1,
            input_channels: 1,
            input_height: 2,
            input_width: 2,
            kernel_height: 2,
            kernel_width: 2,
            stride_height: 1,
            stride_width: 1,
            padding_height: 0,
            padding_width: 0,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid CNN2D model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("2-D convolution should execute");
    assert_eq!(result.entries().len(), 1);
    assert!((result.entries()[0].payload.score - sigmoid(10.0)).abs() < 1e-9);
}

#[test]
fn cnn_2d_uses_channel_vectors_per_spatial_position() {
    let layers = vec![
        Layer::Embed(vec![1.0, 2.0]),
        Layer::Dense {
            weights: vec![1.0, 0.5],
            bias: vec![0.0, 0.0],
            output_channels: 2,
            input_channels: 1,
        },
        Layer::CNN2D {
            weights: vec![0.01, 0.02, 0.03, 0.04],
            bias: vec![0.0],
            output_channels: 1,
            input_channels: 2,
            input_height: 1,
            input_width: 2,
            kernel_height: 1,
            kernel_width: 2,
            stride_height: 1,
            stride_width: 1,
            padding_height: 0,
            padding_width: 0,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None)
        .expect("valid multi-channel CNN2D model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("multi-channel 2-D convolution should execute");
    assert_eq!(result.entries().len(), 1);
    assert!((result.entries()[0].payload.score - sigmoid(0.12)).abs() < 1e-9);
}

#[test]
fn rnn_returns_sequence_hidden_states() {
    let layers = vec![
        Layer::Embed(vec![1.0, 1.0]),
        Layer::RNN {
            weights_input: vec![1.0],
            weights_hidden: vec![1.0],
            bias: vec![0.0],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences: true,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid RNN model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("RNN deep fusion should execute");
    let first_hidden = 1.0_f64.tanh();
    let second_hidden = (1.0 + first_hidden).tanh();
    let scores: BTreeMap<u64, f64> = result
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    assert!((scores[&1] - sigmoid(first_hidden)).abs() < 1e-9);
    assert!((scores[&2] - sigmoid(second_hidden)).abs() < 1e-9);
}

#[test]
fn lstm_accumulates_cell_state() {
    let layers = vec![
        Layer::Embed(vec![1.0, 1.0]),
        Layer::LSTM {
            weights_input: vec![10.0, 10.0, 1.0, 10.0],
            weights_hidden: vec![0.0; 4],
            bias: vec![0.0; 4],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences: true,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid LSTM model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("LSTM deep fusion should execute");
    let scores: Vec<f64> = result.entries().iter().map(|e| e.payload.score).collect();
    assert_eq!(scores.len(), 2);
    assert!(scores[1] > scores[0], "{scores:?}");
}

#[test]
fn propagate_lifts_neighbor_scores_into_residual() {
    // Build: signal entry on doc 1 (score 0.9). Doc 2 has no
    // signal. Add an edge 1->2 in the graph. After Propagate, doc
    // 2 should pick up a positive logit residual.
    let signal = Arc::new(ConstOperator(vec![entry(1, 0.9)])) as Arc<dyn Operator>;
    let mut graph = StaticGraph::new();
    graph.add(1, "knows", 2);
    let ctx = ExecutionContext::new().with_graph(Arc::new(graph));
    let layers = vec![
        Layer::Signal(vec![signal]),
        Layer::Propagate {
            edge_label: "knows".into(),
            aggregation: AggregationKind::Mean,
            direction: Direction::Both,
        },
    ];
    let op =
        DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid graph-propagation model");
    let result = op.execute(&ctx).expect("graph propagation should execute");
    let scores: BTreeMap<u64, f64> = result
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    assert!(scores.contains_key(&2));
    // Doc 2 had no signal, so its prior was 0. After propagation
    // from doc 1's neighbor probability (~0.9), the sigmoid score
    // on channel 0 should be > 0.5.
    assert!(scores[&2] > 0.5, "{scores:?}");
}

#[test]
fn empty_edge_label_propagates_across_every_label() {
    let signal = Arc::new(ConstOperator(vec![entry(1, 0.9)])) as Arc<dyn Operator>;
    let mut graph = StaticGraph::new();
    graph.add(1, "knows", 2);
    graph.add(1, "likes", 3);
    let context = ExecutionContext::new().with_graph(Arc::new(graph));
    let layers = vec![
        Layer::Signal(vec![signal]),
        Layer::Propagate {
            edge_label: String::new(),
            aggregation: AggregationKind::Mean,
            direction: Direction::Both,
        },
    ];
    let operator = DeepFusionOperator::new(layers, 0.0, Gating::None)
        .expect("the empty edge-label wildcard is a valid model");
    let result = operator
        .execute(&context)
        .expect("the wildcard must traverse every edge label");
    let ids: Vec<u64> = result.entries().iter().map(|entry| entry.doc_id).collect();
    assert!(ids.contains(&2), "{ids:?}");
    assert!(ids.contains(&3), "{ids:?}");
}

#[test]
fn pool_groups_neighbors_into_representative() {
    // Doc 1 and 2 are connected; pool groups them together. Doc 3
    // is isolated and forms its own group.
    let signal = Arc::new(ConstOperator(vec![
        entry(1, 0.8),
        entry(2, 0.6),
        entry(3, 0.5),
    ])) as Arc<dyn Operator>;
    let mut graph = StaticGraph::new();
    graph.add(1, "k", 2);
    let ctx = ExecutionContext::new().with_graph(Arc::new(graph));
    let layers = vec![
        Layer::Signal(vec![signal]),
        Layer::Pool {
            edge_label: "k".into(),
            pool_size: 2,
            method: PoolMethod::Avg,
            direction: Direction::Both,
        },
    ];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid graph-pool model");
    let result = op.execute(&ctx).expect("graph pooling should execute");
    let ids: Vec<u64> = result.entries().iter().map(|e| e.doc_id).collect();
    // After pooling, the representatives are {1, 3}; doc 2 is
    // absorbed into doc 1's group.
    assert!(ids.contains(&1));
    assert!(ids.contains(&3));
    assert!(!ids.contains(&2));
}

#[test]
fn attention_keeps_node_count_and_normalises_within_node() {
    let layers = vec![Layer::Embed(vec![1.0, 0.5, -0.5]), Layer::Attention];
    let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid attention model");
    let result = op
        .execute(&ExecutionContext::new())
        .expect("attention deep fusion should execute");
    let ids: Vec<u64> = result.entries().iter().map(|e| e.doc_id).collect();
    assert_eq!(ids.len(), 3);
    // Self-attention over single-channel inputs leaves a weighted
    // mix of the same three values; sigmoids of those mixes should
    // remain in [0, 1].
    for entry in result.entries() {
        assert!((0.0..=1.0).contains(&entry.payload.score));
    }
}

#[test]
fn invalid_model_shapes_are_rejected_at_construction() {
    let error = DeepFusionOperator::new(
        vec![
            Layer::Embed(vec![1.0, 2.0]),
            Layer::Dense {
                weights: vec![1.0],
                bias: vec![0.0],
                output_channels: 1,
                input_channels: 2,
            },
        ],
        0.0,
        Gating::None,
    )
    .err()
    .expect("a truncated dense matrix must be rejected");
    assert!(error
        .to_string()
        .contains("weights has length 1, expected 2"));
}

#[test]
fn feature_models_reject_wrong_width_and_non_finite_inputs() {
    let operator = DeepFusionOperator::new(vec![Layer::Input { dimensions: 2 }], 0.0, Gating::None)
        .expect("valid feature model");
    let width_error = operator
        .execute_features(1, vec![1.0], &ExecutionContext::new())
        .expect_err("wrong-width features must not be padded");
    assert!(width_error.to_string().contains("dimension 1, expected 2"));

    let finite_error = operator
        .execute_features(1, vec![1.0, f64::NAN], &ExecutionContext::new())
        .expect_err("non-finite features must not reach inference");
    assert!(finite_error.to_string().contains("must be finite"));
}

#[test]
fn graph_layers_require_an_execution_graph() {
    let signal = Arc::new(ConstOperator(vec![entry(1, 0.8)])) as Arc<dyn Operator>;
    let operator = DeepFusionOperator::new(
        vec![
            Layer::Signal(vec![signal]),
            Layer::Propagate {
                edge_label: "knows".into(),
                aggregation: AggregationKind::Mean,
                direction: Direction::Out,
            },
        ],
        0.0,
        Gating::None,
    )
    .expect("valid graph model");
    let error = operator
        .execute(&ExecutionContext::new())
        .expect_err("missing graph context must not become a no-op");
    assert!(error.to_string().contains("requires an execution graph"));
}

#[test]
fn non_finite_signal_scores_are_execution_errors() {
    let signal = Arc::new(ConstOperator(vec![entry(1, f64::NAN)])) as Arc<dyn Operator>;
    let operator = DeepFusionOperator::new(vec![Layer::Signal(vec![signal])], 0.0, Gating::None)
        .expect("valid signal model");
    let error = operator
        .execute(&ExecutionContext::new())
        .expect_err("non-finite signal output must not become a result score");
    assert!(error.to_string().contains("finite probability"));

    let out_of_range = Arc::new(ConstOperator(vec![entry(1, 1.5)])) as Arc<dyn Operator>;
    let operator =
        DeepFusionOperator::new(vec![Layer::Signal(vec![out_of_range])], 0.0, Gating::None)
            .expect("valid signal model");
    assert!(operator.execute(&ExecutionContext::new()).is_err());
}
