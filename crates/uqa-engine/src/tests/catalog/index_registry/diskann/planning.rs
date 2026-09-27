//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::VectorQueryRoute;
use uqa_operators::OperatorTree;
use uqa_planner::retrieval_planning::{query_optimizer, RetrievalPlanningCatalog};

fn tree() -> OperatorTree {
    OperatorTree::Intersect(vec![
        OperatorTree::KNN {
            field: "embedding".into(),
            query_vector: vec![1.0, 0.0],
            k: 2,
        },
        OperatorTree::KNN {
            field: "other".into(),
            query_vector: vec![1.0, 0.0, 0.0],
            k: 2,
        },
        OperatorTree::KNN {
            field: "embedding".into(),
            query_vector: vec![0.0, 0.0],
            k: 2,
        },
    ])
}

fn check(engine: &Engine, populations: Option<(u64, u64)>) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2), other vector(3)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0],ARRAY[0.0,1.0]],ARRAY[1.0,0.0,0.0]),(2,ARRAY[ARRAY[0.0,0.0]],ARRAY[0.0,1.0,0.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(max_degree=2, search_list_size=4, beam_width=2, pq_bytes=1); CREATE INDEX diskann_other ON diskann_docs USING diskann(other) WITH(max_degree=2, search_list_size=8, beam_width=1, pq_bytes=3)");
    let query = tree();
    let optimizer = query_optimizer(engine, "diskann_docs", &query).unwrap();
    let stats = &optimizer.index_stats;
    let first = stats.diskann_query("embedding", &[1.0, 0.0]).unwrap();
    let zero = stats.diskann_query("embedding", &[0.0, 0.0]).unwrap();
    let other = stats.diskann_query("other", &[1.0, 0.0, 0.0]).unwrap();
    assert_eq!(first.query_route, VectorQueryRoute::Approximate);
    assert_eq!(zero.query_route, VectorQueryRoute::ExactZeroNorm);
    assert_eq!(first.index.generation, zero.index.generation);
    // Memory indexes use independent incarnations with the same local index ordinal.
    assert_ne!(first.index.generation, other.index.generation);
    assert_eq!((first.index.dimensions, other.index.dimensions), (2, 3));
    assert_eq!((first.index.pq_bytes, other.index.pq_bytes), (1, 3));
    assert_eq!((first.index.beam_width, other.index.beam_width), (2, 1));
    assert_eq!(
        (
            first.index.populations.base_vectors,
            first.index.populations.side_vectors
        ),
        (3, 1)
    );
    assert_eq!(
        (
            first.index.populations.current_vectors,
            first.index.populations.changed_vectors
        ),
        (
            populations.map(|counts| counts.0),
            populations.map(|counts| counts.1)
        )
    );
    let fixed = engine.capture_statement_read_snapshot().unwrap();
    let reader = engine.statement_read_snapshot_engine(&fixed);
    sql(engine, "DROP INDEX diskann_other; CREATE INDEX diskann_other ON diskann_docs USING diskann(other) WITH(max_degree=2, search_list_size=16, beam_width=2, pq_bytes=1)");
    let retained = query_optimizer(&reader, "diskann_docs", &query).unwrap();
    assert_eq!(
        retained
            .index_stats
            .diskann_query("other", &[1.0, 0.0, 0.0]),
        Some(other)
    );
    let live = query_optimizer(engine, "diskann_docs", &query).unwrap();
    let changed = live
        .index_stats
        .diskann_query("other", &[1.0, 0.0, 0.0])
        .unwrap();
    assert_ne!(changed.index.generation, other.index.generation);
    assert_eq!(
        (changed.index.search_list_size, changed.index.pq_bytes),
        (16, 1)
    );
    assert_search(engine, &[(1, 1.0), (2, 0.0)]);
}

#[test]
fn diskann_planning_retains_per_field_and_query_facts_on_all_providers() {
    check(&Engine::new(), Some((3, 0)));
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        check(&engine, None);
    }
}

#[test]
fn diskann_planning_preserves_retained_cancellation_sqlstate() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE diskann_docs(embedding vector(2)); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    let table = RetrievalPlanningCatalog::try_query_table(&engine, "diskann_docs")
        .unwrap()
        .unwrap();
    engine.runtime.cancellation.cancel();
    let error = table
        .vector_indexes()
        .diskann_query_statistics("embedding", &[1.0, 0.0])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn diskann_planning_preserves_vector_dimension_diagnostics() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0])");
    let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0],1)";
    let original = engine.sql(query, &[]).unwrap_err();
    assert_eq!(original.sqlstate(), Some("42804"));
    assert!(
        matches!(&original, uqa_sql::SQLError::TypeMismatch(message) if message == "vector query for \"embedding\" has 1 dimensions, expected 2")
    );
    sql(
        &engine,
        "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
    );
    let physical = engine.sql(query, &[]).unwrap_err();
    assert_eq!(physical.sqlstate(), original.sqlstate());
    assert_eq!(physical.to_string(), original.to_string());
}
