//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL parser/compiler benchmarks mirroring UQA `bench_compiler.py`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fmt::Write;
use uqa_engine::Engine;

fn compile(sql: &str) -> usize {
    uqa_sql::compile(sql).expect("compile").len()
}

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, category TEXT, value INTEGER)",
            &[],
        )
        .expect("create bench");
    engine
        .sql(
            "CREATE TABLE dept (id INTEGER PRIMARY KEY, category TEXT, label TEXT)",
            &[],
        )
        .expect("create dept");
    engine
        .sql(
            "CREATE TABLE bench_dml (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)",
            &[],
        )
        .expect("create dml");
    engine
        .sql(
            "CREATE TABLE bench_dml_insert (id INTEGER, name TEXT, value INTEGER)",
            &[],
        )
        .expect("create dml insert");
    let mut values = String::from("INSERT INTO bench (id, name, category, value) VALUES ");
    for i in 0..1_000 {
        if i > 0 {
            values.push_str(", ");
        }
        let _ = write!(
            values,
            "({i}, 'name_{i}', 'cat_{}', {})",
            i % 10,
            i % 10_000
        );
    }
    engine.sql(&values, &[]).expect("insert bench");
    let mut dept = String::from("INSERT INTO dept (id, category, label) VALUES ");
    for i in 0..10 {
        if i > 0 {
            dept.push_str(", ");
        }
        let _ = write!(dept, "({i}, 'cat_{i}', 'label_{i}')");
    }
    engine.sql(&dept, &[]).expect("insert dept");
    engine
}

fn bench_parse(c: &mut Criterion) {
    let cases = [
        ("parse_simple_select", "SELECT id, name FROM bench WHERE value > 100"),
        (
            "parse_complex_join",
            "SELECT b.id, d.label FROM bench b JOIN dept d ON b.category = d.category WHERE b.value > 100 ORDER BY b.id LIMIT 50",
        ),
        (
            "parse_subquery",
            "SELECT id FROM bench WHERE value > (SELECT AVG(value) FROM bench)",
        ),
        (
            "parse_cte",
            "WITH filtered AS (SELECT * FROM bench WHERE value > 100) SELECT category, COUNT(*) FROM filtered GROUP BY category",
        ),
        (
            "parse_window_function",
            "SELECT id, ROW_NUMBER() OVER (PARTITION BY category ORDER BY value DESC) AS rn FROM bench",
        ),
    ];
    let mut group = c.benchmark_group("sql_parse_compile");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| black_box(compile(black_box(sql))));
        });
    }
    group.finish();
}

fn bench_compile_select(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "compile_select_simple",
            "SELECT id, name FROM bench WHERE value > 100",
        ),
        (
            "compile_select_multiple_predicates",
            "SELECT id FROM bench WHERE value > 100 AND category = 'cat_5'",
        ),
        (
            "compile_select_with_expressions",
            "SELECT id, value + 10 AS adjusted FROM bench WHERE value * 2 > 300",
        ),
    ];
    let mut group = c.benchmark_group("sql_compile_select_execute");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("select");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

fn bench_compile_join_aggregate_subquery(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "compile_join_2way",
            "SELECT b.id, d.label FROM bench b JOIN dept d ON b.category = d.category",
        ),
        (
            "compile_join_3way",
            "SELECT b1.id, b2.id, d.label FROM bench b1 JOIN bench b2 ON b1.category = b2.category JOIN dept d ON b1.category = d.category WHERE b1.id < 10",
        ),
        (
            "compile_group_by",
            "SELECT category, COUNT(*) FROM bench GROUP BY category",
        ),
        (
            "compile_group_by_having",
            "SELECT category, COUNT(*) AS c FROM bench GROUP BY category HAVING COUNT(*) > 10",
        ),
        (
            "compile_scalar_subquery",
            "SELECT id FROM bench WHERE value > (SELECT AVG(value) FROM bench)",
        ),
        (
            "compile_exists_subquery",
            "SELECT id FROM bench b WHERE EXISTS (SELECT 1 FROM dept d WHERE d.category = b.category)",
        ),
        (
            "compile_single_cte",
            "WITH filtered AS (SELECT * FROM bench WHERE value > 100) SELECT COUNT(*) FROM filtered",
        ),
        (
            "compile_multiple_ctes",
            "WITH a AS (SELECT * FROM bench WHERE value > 100), b AS (SELECT * FROM a WHERE category = 'cat_1') SELECT COUNT(*) FROM b",
        ),
        (
            "compile_row_number",
            "SELECT id, ROW_NUMBER() OVER (ORDER BY value DESC) AS rn FROM bench",
        ),
        (
            "compile_partition_window",
            "SELECT id, RANK() OVER (PARTITION BY category ORDER BY value DESC) AS rk FROM bench",
        ),
    ];
    let mut group = c.benchmark_group("sql_compile_relational_execute");
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

fn bench_compile_dml(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("compile_dml_insert", |bencher| {
        let mut next_id = 10_000_i64;
        bencher.iter(|| {
            let sql = format!(
                "INSERT INTO bench_dml_insert (id, name, value) VALUES ({next_id}, 'inserted', 1)"
            );
            next_id += 1;
            let result = engine.sql(black_box(&sql), &[]).expect("insert");
            black_box(result.rows.len())
        });
    });
    c.bench_function("compile_dml_update", |bencher| {
        bencher.iter(|| {
            let result = engine
                .sql(
                    black_box("UPDATE bench SET value = value + 1 WHERE id = 500"),
                    &[],
                )
                .expect("update");
            black_box(result.rows.len())
        });
    });
    c.bench_function("compile_dml_delete", |bencher| {
        let mut next_id = 20_000_i64;
        bencher.iter(|| {
            let insert =
                format!("INSERT INTO bench_dml (id, name, value) VALUES ({next_id}, 'deleted', 1)");
            let delete = format!("DELETE FROM bench_dml WHERE id = {next_id}");
            next_id += 1;
            engine.sql(&insert, &[]).expect("insert before delete");
            let result = engine.sql(black_box(&delete), &[]).expect("delete");
            black_box(result.rows.len())
        });
    });
}

criterion_group!(
    benches,
    bench_parse,
    bench_compile_select,
    bench_compile_join_aggregate_subquery,
    bench_compile_dml
);
criterion_main!(benches);
