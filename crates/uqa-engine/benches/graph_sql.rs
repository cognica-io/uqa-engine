//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL graph-function benchmarks for centrality, bounded RPQs, and named graphs.

use std::fmt::Write as _;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::{Edge, Value, Vertex};
use uqa_engine::Engine;
use uqa_graph::GraphStore;

const GRAPH: &str = "bench";

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE seeds (id INTEGER PRIMARY KEY, name TEXT)",
            &[],
        )
        .expect("create seeds");
    let mut values = String::from("INSERT INTO seeds (id, name) VALUES ");
    for id in 1..=500 {
        if id > 1 {
            values.push_str(", ");
        }
        let _ = write!(values, "({id}, 'v_{id}')");
    }
    engine.sql(&values, &[]).expect("insert seeds");
    engine.create_graph(GRAPH).unwrap();
    engine
        .graph_with_mut(GRAPH, |store| {
            store.create_graph(GRAPH)?;
            for id in 1..=500 {
                store.add_vertex(Vertex::new(id, "Person"), GRAPH)?;
            }
            let mut edge_id = 1;
            for id in 1..500 {
                let mut edge = Edge::new(edge_id, id, id + 1, "knows");
                edge.properties
                    .insert("weight".to_string(), Value::Float((id % 10) as f64 / 10.0));
                edge.properties
                    .insert("valid_from".to_string(), Value::Int(0));
                edge.properties
                    .insert("valid_to".to_string(), Value::Int(1_000));
                store.add_edge(edge, GRAPH)?;
                edge_id += 1;
            }
            store.add_edge(Edge::new(edge_id, 2, 3, "works_at"), GRAPH)?;
            edge_id += 1;
            for id in 1_u64..=450 {
                if id.is_multiple_of(25) {
                    store.add_edge(Edge::new(edge_id, id, id + 50, "knows"), GRAPH)?;
                    edge_id += 1;
                }
            }
            Ok(())
        })
        .expect("graph storage")
        .expect("graph exists");
    engine
}

fn bench_centrality_sql(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "graph_sql_pagerank",
            "SELECT _doc_id, _score FROM pagerank() ORDER BY _score DESC LIMIT 10",
        ),
        (
            "graph_sql_hits",
            "SELECT _doc_id, _score FROM hits() ORDER BY _score DESC LIMIT 10",
        ),
        (
            "graph_sql_betweenness",
            "SELECT _doc_id, _score FROM betweenness() ORDER BY _score DESC LIMIT 10",
        ),
        (
            "graph_sql_pagerank_where",
            "SELECT name, _score FROM seeds WHERE pagerank() ORDER BY _score DESC LIMIT 5",
        ),
    ];
    let mut group = c.benchmark_group("graph_centrality_sql");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("graph sql");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_rpq_sql(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("graph_sql_bounded_rpq", |bencher| {
        bencher.iter(|| {
            let result = engine
                .sql(
                    black_box("SELECT COUNT(*) AS cnt FROM rpq('knows{1,2}', 1)"),
                    &[],
                )
                .expect("rpq");
            black_box(result.rows.len())
        });
    });
}

fn bench_named_graph_sql(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "graph_sql_named_traverse",
            "SELECT id FROM seeds WHERE graph_traverse('bench', 1, 'knows', 2) ORDER BY id",
        ),
        (
            "graph_sql_named_temporal_traverse",
            "SELECT id FROM seeds WHERE temporal_traverse('bench', 1, 'knows', 2, 100, 200) ORDER BY id",
        ),
        (
            "graph_sql_named_rpq",
            "SELECT * FROM rpq('knows/works_at', 1, 'bench')",
        ),
    ];
    for (_, sql) in cases {
        let result = engine.sql(sql, &[]).expect("named graph SQL setup");
        assert!(
            !result.rows.is_empty(),
            "benchmark query returned no rows: {sql}"
        );
    }

    let mut group = c.benchmark_group("graph_named_sql");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("named graph SQL");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn build_persistent_engine(size: u64) -> (tempfile::TempDir, Engine) {
    let directory = tempfile::tempdir().expect("temporary graph fixture");
    let engine = Engine::open(&directory.path().join("graph.db")).expect("open graph fixture");
    engine.create_graph(GRAPH).expect("create graph");
    engine
        .graph_with_mut(GRAPH, |store| {
            for id in 1..=size {
                let mut vertex = Vertex::new(id, "Item");
                vertex.properties.insert(
                    "body".into(),
                    Value::Str("synthetic graph property".repeat(16)),
                );
                store.add_vertex(vertex, GRAPH)?;
            }
            // Keep the queried neighborhood fixed as unrelated graph data grows.
            store.add_edge(Edge::new(1, 1, 2, "next"), GRAPH)?;
            for id in 3..size {
                store.add_edge(Edge::new(id, id, id + 1, "unrelated"), GRAPH)?;
            }
            Ok(())
        })
        .expect("persist graph fixture")
        .expect("graph exists");
    (directory, engine)
}

fn bench_persistent_graph_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("persistent_graph_scaling");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(1));
    for size in [1_024_u64, 65_536] {
        let (directory, engine) = build_persistent_engine(size);
        group.bench_function(BenchmarkId::new("open", size), |bencher| {
            bencher.iter(|| {
                black_box(
                    Engine::open(&directory.path().join("graph.db")).expect("reopen graph fixture"),
                );
            });
        });
        group.bench_function(BenchmarkId::new("new_session", size), |bencher| {
            bencher.iter(|| {
                black_box(engine.new_session().expect("graph session"));
            });
        });
        group.bench_function(BenchmarkId::new("one_hop", size), |bencher| {
            bencher.iter(|| {
                let neighbors = engine
                    .graph_with(GRAPH, |store| {
                        store.neighbors(1, Some("next"), uqa_graph::Direction::Out, GRAPH)
                    })
                    .expect("graph snapshot")
                    .expect("graph exists")
                    .expect("one-hop query");
                assert_eq!(neighbors, vec![2]);
                black_box(neighbors)
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_centrality_sql,
    bench_rpq_sql,
    bench_named_graph_sql,
    bench_persistent_graph_scaling
);
criterion_main!(benches);
