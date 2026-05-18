//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL execution benchmarks ported from UQA `bench_execution.py`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::fmt::Write;
use uqa_engine::Engine;

fn build_engine(n: u64) -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, category TEXT, value INTEGER)",
            &[],
        )
        .expect("create");
    let mut values = String::from("INSERT INTO bench (id, name, category, value) VALUES ");
    for i in 0..n {
        if i > 0 {
            values.push_str(", ");
        }
        let _ = write!(
            values,
            "({i}, 'name_{i}', 'cat_{}', {})",
            i % 20,
            i % 10_000
        );
    }
    engine.sql(&values, &[]).expect("insert");
    engine
}

fn bench_seq_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("execution_seq_scan_full");
    for n in [100_u64, 500, 1_000] {
        let engine = build_engine(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, _| {
            bencher.iter(|| {
                let result = engine
                    .sql(black_box("SELECT * FROM bench"), &[])
                    .expect("scan");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_filter_project_sort(c: &mut Criterion) {
    let engine = build_engine(10_000);
    let cases = [
        (
            "execution_filter_high_selectivity",
            "SELECT * FROM bench WHERE value > 500",
        ),
        (
            "execution_filter_low_selectivity",
            "SELECT * FROM bench WHERE value > 9900",
        ),
        (
            "execution_filter_compound",
            "SELECT * FROM bench WHERE value > 500 AND category = 'cat_5'",
        ),
        (
            "execution_project_simple",
            "SELECT id, name, value FROM bench",
        ),
        (
            "execution_project_expr",
            "SELECT id, value + 10 AS plus_ten, value * 2 AS doubled FROM bench",
        ),
        (
            "execution_sort_single_column",
            "SELECT * FROM bench ORDER BY value",
        ),
        (
            "execution_sort_multi_column",
            "SELECT * FROM bench ORDER BY category, value DESC",
        ),
        (
            "execution_sort_with_limit",
            "SELECT * FROM bench ORDER BY value DESC LIMIT 10",
        ),
    ];
    let mut group = c.benchmark_group("execution_filter_project_sort");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("query");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_aggregate_distinct_window(c: &mut Criterion) {
    let engine = build_engine(10_000);
    let cases = [
        (
            "execution_hash_aggregate_count",
            "SELECT category, COUNT(*) FROM bench GROUP BY category",
        ),
        (
            "execution_hash_aggregate_sum_avg",
            "SELECT category, SUM(value), AVG(value) FROM bench GROUP BY category",
        ),
        (
            "execution_hash_aggregate_high_cardinality",
            "SELECT name, COUNT(*) FROM bench GROUP BY name",
        ),
        (
            "execution_distinct_low_cardinality",
            "SELECT DISTINCT category FROM bench",
        ),
        (
            "execution_distinct_high_cardinality",
            "SELECT DISTINCT name FROM bench",
        ),
        (
            "execution_window_row_number",
            "SELECT id, ROW_NUMBER() OVER (ORDER BY value DESC) AS rn FROM bench",
        ),
        (
            "execution_window_rank_partitioned",
            "SELECT id, RANK() OVER (PARTITION BY category ORDER BY value DESC) AS rk FROM bench",
        ),
        (
            "execution_window_sum",
            "SELECT id, SUM(value) OVER (PARTITION BY category ORDER BY id) AS running FROM bench",
        ),
    ];
    let mut group = c.benchmark_group("execution_aggregate_distinct_window");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("query");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_limit_pipeline(c: &mut Criterion) {
    let engine = build_engine(10_000);
    let mut limit_group = c.benchmark_group("execution_limit");
    for limit in [10_u64, 100] {
        let sql = format!("SELECT * FROM bench LIMIT {limit}");
        limit_group.bench_with_input(BenchmarkId::from_parameter(limit), &limit, |bencher, _| {
            bencher.iter(|| {
                let result = engine.sql(black_box(&sql), &[]).expect("limit");
                black_box(result.rows.len())
            });
        });
    }
    limit_group.finish();

    let cases = [
        (
            "execution_pipeline_scan_filter_project_sort_limit",
            "SELECT id, name FROM bench WHERE value > 500 ORDER BY value DESC LIMIT 100",
        ),
        (
            "execution_pipeline_scan_group_sort",
            "SELECT category, COUNT(*) AS c FROM bench GROUP BY category ORDER BY c DESC",
        ),
    ];
    let mut group = c.benchmark_group("execution_pipeline");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("pipeline");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_seq_scan,
    bench_filter_project_sort,
    bench_aggregate_distinct_window,
    bench_limit_pipeline
);
criterion_main!(benches);
