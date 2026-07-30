//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular path query benchmark.
//!
//! Builds a 1000-vertex chain graph (`1 -knows-> 2 -knows-> ...`) and
//! runs `knows*` from vertex 1, plus a wider unbounded all-pairs run
//! for comparison.
//!
//! Run with `cargo bench -p uqa-graph --bench rpq`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::{Edge, Vertex};
use uqa_graph::{parse_rpq, GraphStore, MemoryGraphStore, RegularPathQuery};

const N: u64 = 1_000;

fn chain_graph() -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=N {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    for v in 1..N {
        g.add_edge(Edge::new(v + 10_000, v, v + 1, "knows"), "g")
            .unwrap();
    }
    g
}

fn bench_rpq_kleene_star_from_vertex(c: &mut Criterion) {
    let g = chain_graph();
    let expr = parse_rpq("knows*").expect("parse");
    c.bench_function("rpq_kleene_star_from_vertex_1k", |bencher| {
        bencher.iter(|| {
            let pl = RegularPathQuery::new(expr.clone(), "g")
                .from_vertex(1)
                .execute(&g)
                .unwrap();
            black_box(pl.inner().len())
        });
    });
}

fn bench_rpq_concat(c: &mut Criterion) {
    let g = chain_graph();
    let expr = parse_rpq("knows/knows/knows").expect("parse");
    c.bench_function("rpq_concat_3hop_1k", |bencher| {
        bencher.iter(|| {
            let pl = RegularPathQuery::new(expr.clone(), "g")
                .from_vertex(1)
                .execute(&g)
                .unwrap();
            black_box(pl.inner().len())
        });
    });
}

criterion_group!(benches, bench_rpq_kleene_star_from_vertex, bench_rpq_concat);
criterion_main!(benches);
