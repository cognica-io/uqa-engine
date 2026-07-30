//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL graph function benchmarks mirroring the centrality SQL portion
//! of UQA `bench_graph_centrality.py`.

use std::fmt::Write as _;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
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
            store.create_graph(GRAPH);
            for id in 1..=500 {
                store.add_vertex(Vertex::new(id, "Person"), GRAPH)?;
            }
            let mut edge_id = 1;
            for id in 1..500 {
                let mut edge = Edge::new(edge_id, id, id + 1, "knows");
                edge.properties
                    .insert("weight".to_string(), Value::Float((id % 10) as f64 / 10.0));
                store.add_edge(edge, GRAPH)?;
                edge_id += 1;
            }
            for id in 1..=450 {
                if id % 25 == 0 {
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

criterion_group!(benches, bench_centrality_sql, bench_rpq_sql);
criterion_main!(benches);
