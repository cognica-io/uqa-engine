//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_core::Predicate;
use uqa_core::{Edge, Value, Vertex};
use uqa_operators::OperatorTree;
use uqa_planner::retrieval_planning::query_optimizer;
use uqa_planner::retrieval_planning::RetrievalPlanningCatalog;

#[test]
fn costing_index_readers_retain_guards_and_table_generation() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE costing_docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
        "CREATE INDEX costing_text ON costing_docs USING gin (body)",
        "CREATE INDEX costing_vectors ON costing_docs USING hnsw (embedding)",
        "INSERT INTO costing_docs VALUES (1, 'retained', ARRAY[1.0, 0.0])",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let original = engine.try_query_table("costing_docs").unwrap().unwrap();
    let retained = RetrievalPlanningCatalog::try_query_table(&engine, "costing_docs")
        .unwrap()
        .unwrap();
    {
        let text = retained.text_index();
        let terms = text.analyze("body", "retained").unwrap();
        assert!(!terms.is_empty());
        for term in terms {
            assert_eq!(text.doc_freq("body", &term).unwrap(), 1);
        }
        assert!(original.inverted_index.try_write().is_none());
    }
    assert!(original.inverted_index.try_write().is_some());
    {
        let vectors = retained.vector_indexes();
        assert_eq!(vectors.dimensions("embedding"), Some(2));
        assert!(original.vector_indexes.try_write().is_none());
    }
    assert!(original.vector_indexes.try_write().is_some());
    engine.sql("DROP TABLE costing_docs", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE costing_docs (body TEXT, embedding VECTOR(3))",
            &[],
        )
        .unwrap();
    let replacement = engine.try_query_table("costing_docs").unwrap().unwrap();
    assert!(!Arc::ptr_eq(&original, &replacement));
    {
        let _text = retained.text_index();
        assert!(original.inverted_index.try_write().is_none());
        assert!(replacement.inverted_index.try_write().is_some());
    }
    {
        let _vectors = retained.vector_indexes();
        assert!(original.vector_indexes.try_write().is_none());
        assert!(replacement.vector_indexes.try_write().is_some());
    }
}

#[test]
fn query_optimizer_binds_live_graph_statistics_and_sampler() {
    let engine = Engine::new();
    engine.create_graph("citations").unwrap();
    for (id, year) in [(1, 2024), (2, 2023), (3, 2024)] {
        let mut vertex = Vertex::new(id, "Paper");
        vertex.properties.insert("year".into(), Value::Int(year));
        engine.add_graph_vertex(vertex, "citations").unwrap();
    }
    let mut first = Edge::new(1, 1, 2, "cites");
    first
        .properties
        .insert("valid_from".into(), Value::Float(10.0));
    let mut second = Edge::new(2, 1, 3, "cites");
    second
        .properties
        .insert("valid_to".into(), Value::Float(20.0));
    engine.add_graph_edge(first, "citations").unwrap();
    engine.add_graph_edge(second, "citations").unwrap();

    let tree = OperatorTree::Traverse {
        start_vertex: 1,
        graph: "citations".into(),
        label: Some("cites".into()),
        max_hops: 2,
        vertex_predicate: None,
    };
    let optimizer = query_optimizer(&engine, "", &tree).unwrap();
    let stats = optimizer
        .graph_stats
        .as_ref()
        .expect("live graph statistics must be bound");
    assert_eq!(stats.graph_name, "citations");
    assert_eq!(stats.num_vertices, 3);
    assert_eq!(stats.num_edges, 2);
    assert_eq!(stats.label_counts.get("cites"), Some(&2));
    assert_eq!(stats.vertex_label_counts.get("Paper"), Some(&3));
    assert_eq!(stats.min_timestamp, Some(10.0));
    assert_eq!(stats.max_timestamp, Some(20.0));

    let sampler = optimizer
        .estimator
        .graph_store
        .as_ref()
        .expect("live graph sampler must be bound");
    assert_eq!(sampler.vertex_ids(), vec![1, 2, 3]);
    assert_eq!(sampler.outgoing_edges(1).len(), 2);
    let year_2024: uqa_operators::VertexConstraint =
        Arc::new(|vertex| vertex.properties.get("year") == Some(&Value::Int(2024)));
    assert!(sampler.vertex_satisfies(1, &year_2024));
    assert!(!sampler.vertex_satisfies(2, &year_2024));
}

#[test]
fn query_optimizer_binds_analyzed_column_statistics() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE stats_docs (id INTEGER PRIMARY KEY, category INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO stats_docs (id, category) VALUES \
             (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), \
             (6, 6), (7, 7), (8, 8), (9, 9), (10, 10)",
            &[],
        )
        .unwrap();
    engine.sql("ANALYZE stats_docs", &[]).unwrap();
    let tree = OperatorTree::Filter {
        field: "category".into(),
        predicate: Predicate::Equals(Value::Int(4)),
        source: None,
    };

    let optimizer = query_optimizer(&engine, "stats_docs", &tree).unwrap();
    let category = optimizer
        .estimator
        .column_stats
        .get("category")
        .expect("ANALYZE statistics must reach the operator optimizer");
    assert_eq!(category.distinct_count, 10);
    assert_eq!(category.row_count, 10);
    let cost_category = optimizer
        .cost_model
        .column_stats
        .get("category")
        .expect("ANALYZE statistics must reach the operator cost model");
    assert_eq!(cost_category.distinct_count, 10);
    assert_eq!(cost_category.row_count, 10);
    let estimated = optimizer.estimator.estimate(&tree, &optimizer.index_stats);
    assert!(estimated >= 1.0);
    assert!(estimated < 2.0);
}
