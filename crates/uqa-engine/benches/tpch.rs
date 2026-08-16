//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full 22-query TPC-H-derived SF 0.001 compatibility benchmark.
//!
//! This is a local regression workload, not a compliant or audited TPC-H
//! result. Run with `cargo bench -p uqa-engine --bench tpch`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

#[path = "../tests/support/tpch_fixture.rs"]
mod tpch_fixture;

fn bench_tpch_sf0001(c: &mut Criterion) {
    let engine = tpch_fixture::load_engine();
    let queries = tpch_fixture::load_queries();
    let expected = tpch_fixture::load_expected_results();
    for (index, (query, expected)) in queries.iter().zip(expected).enumerate() {
        let actual = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("TPC-H Q{:02} validation: {error}", index + 1));
        assert_eq!(
            tpch_fixture::canonical_result(&actual),
            expected,
            "TPC-H Q{:02} validation differs from PostgreSQL 18",
            index + 1
        );
    }

    let mut group = c.benchmark_group("tpch_sf0001");
    group.sample_size(10);
    for (index, query) in queries.iter().enumerate() {
        group.bench_with_input(
            BenchmarkId::new("query", index + 1),
            query,
            |bencher, sql| {
                bencher.iter(|| {
                    let result = engine.sql(black_box(sql), &[]).expect("TPC-H query");
                    black_box(result.rows)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_tpch_sf0001);
criterion_main!(benches);
