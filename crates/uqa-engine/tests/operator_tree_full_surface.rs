//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Edge, PostingList, Predicate, Value, Vertex};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_engine::Engine;
use uqa_operators::{
    CountMonoid, DeepFusionAggregation, DeepFusionLayer, DeepFusionPoolMethod, DeepGraphDirection,
    EdgePatternIR, GatingSpec, GraphPatternIR, MaxMonoid, OperatorTree, ProgressiveFusionEntry,
    SumMonoid, TemporalFilterIR, TextScoringMode, VertexPatternIR,
};
use uqa_planner::executor::{OperatorOutput, OperatorTreeDriver};
use uqa_scoring::Scorer;

fn fixture() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (\
                id INTEGER PRIMARY KEY, \
                title TEXT, \
                body TEXT, \
                category TEXT, \
                value INTEGER, \
                embedding VECTOR(2)\
            )",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX docs_fts ON docs USING gin (title, body)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_value_idx ON docs (value)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, title, body, category, value, embedding) VALUES \
             (1, 'rust async',   'tokio runtime', 'A', 10, ARRAY[1.0, 0.0]), \
             (2, 'rust systems', 'memory safety', 'A', 20, ARRAY[0.9, 0.1]), \
             (3, 'python web',   'flask server',  'B', 30, ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();

    engine.create_graph("social").unwrap();
    for (id, category, value) in [(1, "A", 10), (2, "A", 20), (3, "B", 30)] {
        let mut vertex = Vertex::new(id, "Person");
        vertex
            .properties
            .insert("category".into(), Value::Str(category.into()));
        vertex.properties.insert("value".into(), Value::Int(value));
        engine.add_graph_vertex(vertex, "social").unwrap();
    }
    let mut first = Edge::new(1, 1, 2, "follows");
    first
        .properties
        .insert("valid_from".into(), Value::Float(0.0));
    first
        .properties
        .insert("valid_to".into(), Value::Float(10.0));
    first.properties.insert("weight".into(), Value::Float(0.7));
    engine.add_graph_edge(first, "social").unwrap();
    let mut second = Edge::new(2, 2, 3, "follows");
    second
        .properties
        .insert("valid_from".into(), Value::Float(5.0));
    second
        .properties
        .insert("valid_to".into(), Value::Float(20.0));
    second.properties.insert("weight".into(), Value::Float(0.4));
    engine.add_graph_edge(second, "social").unwrap();
    engine
        .add_graph_edge(Edge::new(3, 1, 3, "likes"), "social")
        .unwrap();
    engine
}

fn term(query: &str, field: &str) -> OperatorTree {
    OperatorTree::Term {
        query: query.into(),
        field: Some(field.into()),
        scoring: Some(TextScoringMode::BM25),
    }
}

fn vector(query: [f32; 2], threshold: f32) -> OperatorTree {
    OperatorTree::VectorSimilarity {
        query_vector: query.to_vec(),
        threshold,
        field: "embedding".into(),
    }
}

fn all_docs() -> OperatorTree {
    OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::IsNotNull,
        source: None,
    }
}

fn page_rank() -> OperatorTree {
    OperatorTree::PageRank {
        graph: "social".into(),
    }
}

fn pattern() -> GraphPatternIR {
    GraphPatternIR {
        vertex_patterns: vec![
            VertexPatternIR {
                variable: "a".into(),
                constraints: Vec::new(),
                label: Some("Person".into()),
            },
            VertexPatternIR {
                variable: "b".into(),
                constraints: Vec::new(),
                label: Some("Person".into()),
            },
        ],
        edge_patterns: vec![EdgePatternIR {
            source_var: "a".into(),
            target_var: "b".into(),
            label: Some("follows".into()),
            constraints: Vec::new(),
        }],
    }
}

struct FrequencyScorer;

impl Scorer for FrequencyScorer {
    fn idf(&self, _doc_freq: u64) -> f64 {
        1.0
    }

    fn term_score(&self, term_freq: u64, _doc_length: u64, _doc_freq: u64) -> f64 {
        term_freq as f64
    }

    fn term_score_with_idf(&self, term_freq: u64, _doc_length: u64, idf_value: f64) -> f64 {
        term_freq as f64 * idf_value
    }

    fn finalize_score(&self, term_scores: &[f64]) -> f64 {
        term_scores.iter().sum()
    }

    fn term_upper_bound(&self, _doc_freq: u64) -> f64 {
        1.0
    }
}

fn posting(output: &OperatorOutput) -> PostingList {
    match output {
        OperatorOutput::Posting(posting) => posting.clone(),
        OperatorOutput::Graph(graph) => graph.to_posting_list(),
        OperatorOutput::Generalized(_) => {
            panic!("operator must produce a single-document posting carrier")
        }
    }
}

#[test]
fn primitive_hybrid_and_aggregate_nodes_execute_physically() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    let facet = driver
        .execute_node(&OperatorTree::Facet {
            field: "category".into(),
            source: Some(Box::new(all_docs())),
        })
        .unwrap();
    assert_eq!(facet.len(), 2);

    let score = driver
        .execute_node(&OperatorTree::Score {
            scorer: Arc::new(FrequencyScorer),
            source: Box::new(term("rust", "title")),
            query_terms: vec!["rust".into()],
            field: "title".into(),
        })
        .unwrap();
    assert_eq!(posting(&score).doc_ids().collect::<Vec<_>>(), vec![1, 2]);
    assert!(posting(&score)
        .entries()
        .iter()
        .all(|entry| entry.payload.score > 0.0));

    let vector_result = driver.execute_node(&vector([1.0, 0.0], 0.8)).unwrap();
    assert_eq!(
        posting(&vector_result).doc_ids().collect::<Vec<_>>(),
        vec![1, 2]
    );

    let hybrid = driver
        .execute_node(&OperatorTree::HybridTextVector {
            term_op: Box::new(term("rust", "title")),
            vector_op: Box::new(vector([1.0, 0.0], 0.8)),
            alpha: 0.4,
        })
        .unwrap();
    assert_eq!(posting(&hybrid).doc_ids().collect::<Vec<_>>(), vec![1, 2]);

    let semantic = driver
        .execute_node(&OperatorTree::SemanticFilter {
            source: Box::new(all_docs()),
            vector_op: Box::new(vector([1.0, 0.0], 0.8)),
        })
        .unwrap();
    assert_eq!(posting(&semantic).doc_ids().collect::<Vec<_>>(), vec![1, 2]);

    let aggregate = driver
        .execute_node(&OperatorTree::Aggregate {
            source: Some(Box::new(all_docs())),
            field: "value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap();
    assert_eq!(posting(&aggregate).entries()[0].payload.score, 60.0);

    let groups = driver
        .execute_node(&OperatorTree::GroupBy {
            source: Box::new(all_docs()),
            group_field: "category".into(),
            agg_field: "value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap();
    assert_eq!(groups.len(), 2);

    let indexed = driver
        .execute_node(&OperatorTree::IndexScan {
            index_name: "docs_value_idx".into(),
            field: "value".into(),
            predicate: Predicate::Equals(Value::Int(20)),
        })
        .unwrap();
    assert_eq!(posting(&indexed).doc_ids().collect::<Vec<_>>(), vec![2]);

    let composed = driver
        .execute_node(&OperatorTree::Composed(vec![
            term("rust", "title"),
            OperatorTree::Filter {
                field: "category".into(),
                predicate: Predicate::Equals(Value::Str("B".into())),
                source: None,
            },
        ]))
        .unwrap();
    assert_eq!(posting(&composed).doc_ids().collect::<Vec<_>>(), vec![3]);
}

#[test]
fn graph_temporal_and_centrality_nodes_execute_physically() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    let traverse = driver
        .execute_node(&OperatorTree::Traverse {
            start_vertex: 1,
            graph: "social".into(),
            label: Some("follows".into()),
            max_hops: 2,
            vertex_predicate: None,
        })
        .unwrap();
    assert_eq!(
        posting(&traverse).doc_ids().collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(posting(&traverse).entries()[0]
        .payload
        .fields
        .contains_key("_graph_vertices"));

    let matched = driver
        .execute_node(&OperatorTree::PatternMatch {
            pattern: pattern(),
            graph: "social".into(),
        })
        .unwrap();
    assert_eq!(matched.len(), 2);

    let rpq = driver
        .execute_node(&OperatorTree::RegularPathQuery {
            rpq_source: "follows".into(),
            start_vertex: 1,
            graph: "social".into(),
        })
        .unwrap();
    assert_eq!(posting(&rpq).doc_ids().collect::<Vec<_>>(), vec![2]);

    for tree in [
        page_rank(),
        OperatorTree::HITS {
            graph: "social".into(),
        },
        OperatorTree::BetweennessCentrality {
            graph: "social".into(),
        },
        OperatorTree::MessagePassing {
            source: Box::new(page_rank()),
        },
        OperatorTree::GraphEmbedding {
            source: Box::new(page_rank()),
        },
    ] {
        assert_eq!(driver.execute_node(&tree).unwrap().len(), 3);
    }

    let vertex_aggregate = driver
        .execute_node(&OperatorTree::VertexAggregation {
            source: Box::new(page_rank()),
            monoid: Arc::new(CountMonoid),
        })
        .unwrap();
    assert_eq!(posting(&vertex_aggregate).entries()[0].payload.score, 3.0);

    let temporal = driver
        .execute_node(&OperatorTree::TemporalTraverse {
            start_vertex: 1,
            graph: "social".into(),
            label: Some("follows".into()),
            max_hops: 2,
            temporal_filter: Some(TemporalFilterIR {
                timestamp: Some(2.0),
                time_range: Some((0.0, 4.0)),
            }),
        })
        .unwrap();
    assert_eq!(posting(&temporal).doc_ids().collect::<Vec<_>>(), vec![1, 2]);

    let temporal_pattern = driver
        .execute_node(&OperatorTree::TemporalPatternMatch {
            pattern: pattern(),
            graph: "social".into(),
            temporal_filter: Some(TemporalFilterIR {
                timestamp: Some(2.0),
                time_range: None,
            }),
        })
        .unwrap();
    assert_eq!(temporal_pattern.len(), 1);
}

#[test]
fn graph_set_nodes_preserve_the_graph_carrier_and_merge_subgraphs() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);
    let one_hop = OperatorTree::Traverse {
        start_vertex: 1,
        graph: "social".into(),
        label: Some("follows".into()),
        max_hops: 1,
        vertex_predicate: None,
    };
    let two_hops = OperatorTree::Traverse {
        start_vertex: 1,
        graph: "social".into(),
        label: Some("follows".into()),
        max_hops: 2,
        vertex_predicate: None,
    };

    let union = driver
        .execute_node(&OperatorTree::Union(vec![
            one_hop.clone(),
            two_hops.clone(),
        ]))
        .unwrap();
    let union = union
        .as_graph()
        .expect("a union of graph results must retain the graph carrier");
    assert_eq!(union.inner().doc_ids().collect::<Vec<_>>(), vec![1, 2, 3]);
    let payload = union.get_graph_payload(1).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![1, 2, 3]);
    assert_eq!(payload.subgraph_edges, vec![1, 2]);

    let intersection = driver
        .execute_node(&OperatorTree::Intersect(vec![one_hop, two_hops]))
        .unwrap();
    let intersection = intersection
        .as_graph()
        .expect("an intersection of graph results must retain the graph carrier");
    assert_eq!(
        intersection.inner().doc_ids().collect::<Vec<_>>(),
        vec![1, 2]
    );
    let payload = intersection.get_graph_payload(1).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![1, 2]);
    assert_eq!(payload.subgraph_edges, vec![1]);
}

#[test]
fn weighted_path_executes_its_physical_predicate() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);
    let weighted = driver
        .execute_node(&OperatorTree::WeightedPathQuery {
            rpq_source: "follows".into(),
            start_vertex: 1,
            graph: "social".into(),
            weight_property: "weight".into(),
            default_edge_weight: 1.0,
            max_hops: 2,
            predicate: Arc::new(|weight| weight >= 0.6),
            predicate_selectivity: 0.5,
            score: 0.8,
        })
        .unwrap();

    assert_eq!(posting(&weighted).entries()[0].payload.score, 0.8);
    assert_eq!(
        posting(&weighted).entries()[0]
            .payload
            .fields
            .get("_path_weight"),
        Some(&Value::Float(0.7))
    );
}

#[test]
fn graph_join_set_algebra_preserves_tuple_identity() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    let graph_join = driver
        .execute_node(&OperatorTree::GraphJoin {
            left: Box::new(page_rank()),
            right: Box::new(page_rank()),
            label: Some("follows".into()),
            graph: "social".into(),
        })
        .unwrap();
    let graph_join = graph_join
        .as_generalized()
        .expect("graph join must preserve tuple identity");
    assert_eq!(graph_join.len(), 2);
    assert_eq!(graph_join.entries()[0].doc_ids, vec![1, 2]);

    let joined_union = driver
        .execute_node(&OperatorTree::Union(vec![
            OperatorTree::GraphJoin {
                left: Box::new(page_rank()),
                right: Box::new(page_rank()),
                label: Some("follows".into()),
                graph: "social".into(),
            },
            OperatorTree::GraphJoin {
                left: Box::new(page_rank()),
                right: Box::new(page_rank()),
                label: Some("likes".into()),
                graph: "social".into(),
            },
        ]))
        .unwrap();
    let joined_tuples: Vec<Vec<u64>> = joined_union
        .as_generalized()
        .expect("set algebra over joins must retain the generalized carrier")
        .entries()
        .iter()
        .map(|entry| entry.doc_ids.clone())
        .collect();
    assert_eq!(joined_tuples, vec![vec![1, 2], vec![1, 3], vec![2, 3]]);
}

#[test]
fn similarity_and_cross_paradigm_joins_execute_physically() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    let text_join = driver
        .execute_node(&OperatorTree::TextSimilarityJoin {
            left: Box::new(term("rust", "title")),
            right: Box::new(term("rust", "title")),
            threshold: 0.2,
        })
        .unwrap();
    assert_eq!(
        text_join
            .as_generalized()
            .expect("text join must produce tuple rows")
            .len(),
        4
    );

    let vector_join = driver
        .execute_node(&OperatorTree::VectorSimilarityJoin {
            left: Box::new(vector([1.0, 0.0], -1.0)),
            right: Box::new(vector([1.0, 0.0], -1.0)),
            threshold: 0.8,
        })
        .unwrap();
    assert!(
        vector_join
            .as_generalized()
            .expect("vector join must produce tuple rows")
            .len()
            >= 4
    );

    let hybrid_operand = || {
        OperatorTree::Intersect(vec![
            OperatorTree::Filter {
                field: "category".into(),
                predicate: Predicate::Equals(Value::Str("A".into())),
                source: None,
            },
            vector([1.0, 0.0], -1.0),
        ])
    };
    let hybrid_join = driver
        .execute_node(&OperatorTree::HybridJoin {
            left: Box::new(hybrid_operand()),
            right: Box::new(hybrid_operand()),
        })
        .unwrap();
    assert_eq!(
        hybrid_join
            .as_generalized()
            .expect("hybrid join must produce tuple rows")
            .len(),
        4
    );

    let cross_join = driver
        .execute_node(&OperatorTree::CrossParadigmJoin {
            left: Box::new(page_rank()),
            right: Box::new(OperatorTree::Filter {
                field: "category".into(),
                predicate: Predicate::IsNotNull,
                source: None,
            }),
        })
        .unwrap();
    assert_eq!(
        cross_join
            .as_generalized()
            .expect("cross-paradigm join must produce tuple rows")
            .len(),
        5
    );
}

#[test]
fn layered_fusion_nodes_execute_physically() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);
    let progressive = driver
        .execute_node(&OperatorTree::ProgressiveFusion {
            stages: vec![
                ProgressiveFusionEntry {
                    signal: page_rank(),
                    k: 3,
                },
                ProgressiveFusionEntry {
                    signal: OperatorTree::HITS {
                        graph: "social".into(),
                    },
                    k: 2,
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::Pass,
        })
        .unwrap();
    assert_eq!(progressive.len(), 2);

    let deep = driver
        .execute_node(&OperatorTree::DeepFusion {
            layers: vec![
                DeepFusionLayer::Signal {
                    signals: vec![page_rank()],
                },
                DeepFusionLayer::Propagate {
                    edge_label: Some("follows".into()),
                    aggregation: DeepFusionAggregation::Mean,
                    direction: DeepGraphDirection::Both,
                },
                DeepFusionLayer::Conv {
                    edge_label: None,
                    hop_weights: vec![0.5, 0.5],
                    direction: DeepGraphDirection::Both,
                },
                DeepFusionLayer::Pool {
                    edge_label: None,
                    pool_size: 2,
                    method: DeepFusionPoolMethod::Average,
                    direction: DeepGraphDirection::Both,
                },
                DeepFusionLayer::BatchNorm { epsilon: 1e-5 },
                DeepFusionLayer::Dropout { probability: 0.5 },
                DeepFusionLayer::Flatten,
                DeepFusionLayer::Dense {
                    weights: vec![0.5, 0.5],
                    bias: vec![0.0],
                    output_channels: 1,
                    input_channels: 2,
                },
                DeepFusionLayer::Softmax,
            ],
            alpha: 0.5,
            gating: GatingSpec::Swish,
        })
        .unwrap();
    assert_eq!(deep.len(), 1);
}

fn assert_unknown_and_invalid_layer_errors(driver: &EngineDriver<'_>) {
    let error = driver
        .execute_node(&OperatorTree::Opaque {
            kind: "unregistered_physical_operator".into(),
            children: Vec::new(),
            meta: BTreeMap::new(),
        })
        .expect_err("unknown opaque operators must fail");
    assert!(matches!(error, uqa_sql::SQLError::UnknownFunction(_)));

    let invalid_dense = driver
        .execute_node(&OperatorTree::DeepFusion {
            layers: vec![
                DeepFusionLayer::Signal {
                    signals: vec![page_rank()],
                },
                DeepFusionLayer::Dense {
                    weights: vec![1.0],
                    bias: vec![0.0],
                    output_channels: 2,
                    input_channels: 2,
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::Pass,
        })
        .expect_err("invalid physical layer parameters must fail");
    assert!(matches!(invalid_dense, uqa_sql::SQLError::TypeMismatch(_)));
}

fn assert_invalid_vector_queries_are_errors(driver: &EngineDriver<'_>) {
    let non_vector_field = driver
        .execute_node(&OperatorTree::VectorSimilarity {
            query_vector: vec![1.0, 0.0],
            threshold: 0.0,
            field: "category".into(),
        })
        .expect_err("a non-vector field must not look like an empty vector result");
    assert!(matches!(
        non_vector_field,
        uqa_sql::SQLError::TypeMismatch(_)
    ));

    let wrong_dimensions = driver
        .execute_node(&OperatorTree::KNN {
            query_vector: vec![1.0],
            k: 2,
            field: "embedding".into(),
        })
        .expect_err("a malformed vector query must not look like an empty vector result");
    assert!(matches!(
        wrong_dimensions,
        uqa_sql::SQLError::TypeMismatch(_)
    ));
}

fn assert_missing_graph_starts_are_errors(driver: &EngineDriver<'_>) {
    for graph_node in [
        OperatorTree::GraphNeighbors {
            vertex: 999,
            graph: "social".into(),
            label: None,
            direction: DeepGraphDirection::Out,
        },
        OperatorTree::Traverse {
            start_vertex: 999,
            graph: "social".into(),
            label: None,
            max_hops: 0,
            vertex_predicate: None,
        },
        OperatorTree::RegularPathQuery {
            rpq_source: "follows*".into(),
            start_vertex: 999,
            graph: "social".into(),
        },
        OperatorTree::TemporalTraverse {
            start_vertex: 999,
            graph: "social".into(),
            label: None,
            max_hops: 0,
            temporal_filter: None,
        },
    ] {
        let error = driver
            .execute_node(&graph_node)
            .expect_err("a missing start vertex must not look like an empty graph result");
        assert!(
            matches!(error, uqa_sql::SQLError::Internal(ref message) if message.contains("not a member")),
            "unexpected graph error: {error}"
        );
    }
}

fn assert_graph_aware_fusion_rejects_non_members(driver: &EngineDriver<'_>) {
    let graph_aware_missing_vertex = driver
        .execute_node(&OperatorTree::DeepFusion {
            layers: vec![
                DeepFusionLayer::Signal {
                    signals: vec![
                        page_rank(),
                        OperatorTree::VertexAggregation {
                            source: Box::new(page_rank()),
                            // VertexAggregation emits its scalar at document 0.
                            // Keep that deliberately missing graph id while
                            // preserving DeepFusion's probability contract.
                            monoid: Arc::new(MaxMonoid),
                        },
                    ],
                },
                DeepFusionLayer::Propagate {
                    edge_label: None,
                    aggregation: DeepFusionAggregation::Mean,
                    direction: DeepGraphDirection::Out,
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::Pass,
        })
        .expect_err("graph-aware fusion must reject signal ids outside graph membership");
    assert!(
        matches!(graph_aware_missing_vertex, uqa_sql::SQLError::Internal(ref message) if message.contains("input vertex 0")),
        "unexpected graph-aware fusion error: {graph_aware_missing_vertex}"
    );
}

fn assert_valid_vertex_aggregation_still_executes(driver: &EngineDriver<'_>) {
    let max = driver
        .execute_node(&OperatorTree::VertexAggregation {
            source: Box::new(page_rank()),
            monoid: Arc::new(MaxMonoid),
        })
        .unwrap();
    assert!(posting(&max).entries()[0].payload.score.is_finite());
}

#[test]
fn malformed_physical_nodes_are_errors_not_empty_results() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    assert_unknown_and_invalid_layer_errors(&driver);
    assert_invalid_vector_queries_are_errors(&driver);
    assert_missing_graph_starts_are_errors(&driver);
    assert_graph_aware_fusion_rejects_non_members(&driver);
    assert_valid_vertex_aggregation_still_executes(&driver);
}
