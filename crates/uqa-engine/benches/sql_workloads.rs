//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end SQL workload benchmarks for OLTP, OLAP, joins, and planning.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use uqa_engine::Engine;

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
            "CREATE TABLE dim (id INTEGER PRIMARY KEY, category TEXT, label TEXT)",
            &[],
        )
        .expect("create dim");
    engine
        .sql(
            "CREATE TABLE audit (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .expect("create audit");
    let mut values = String::from("INSERT INTO bench (id, name, category, value) VALUES ");
    for i in 0..5_000 {
        if i > 0 {
            values.push_str(", ");
        }
        let _ = write!(
            values,
            "({i}, 'name_{i}', 'cat_{}', {})",
            i % 50,
            i % 10_000
        );
    }
    engine.sql(&values, &[]).expect("insert bench");
    let mut dim = String::from("INSERT INTO dim (id, category, label) VALUES ");
    for i in 0..50 {
        if i > 0 {
            dim.push_str(", ");
        }
        let _ = write!(dim, "({i}, 'cat_{i}', 'label_{i}')");
    }
    engine.sql(&dim, &[]).expect("insert dim");
    engine
}

fn build_planner_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE bench (
                id INTEGER PRIMARY KEY,
                name TEXT,
                value INTEGER,
                category TEXT,
                quantity INTEGER,
                active BOOLEAN
            )",
            &[],
        )
        .expect("create planner bench");
    let mut values =
        String::from("INSERT INTO bench (id, name, value, category, quantity, active) VALUES ");
    for i in 0_usize..1_000 {
        if i > 0 {
            values.push_str(", ");
        }
        let active = if i.is_multiple_of(2) { "TRUE" } else { "FALSE" };
        let _ = write!(
            values,
            "({i}, 'name_{i}', {}, 'cat_{}', {}, {active})",
            i % 10_000,
            i % 50,
            i % 100
        );
    }
    engine.sql(&values, &[]).expect("insert planner bench");
    engine
}

fn build_operator_join_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE operator_docs (\
                id INTEGER PRIMARY KEY,\
                category TEXT,\
                embedding VECTOR(4)\
            )",
            &[],
        )
        .expect("create operator join corpus");
    let mut values = String::from("INSERT INTO operator_docs (id, category, embedding) VALUES ");
    for i in 0_u32..256 {
        if i > 0 {
            values.push_str(", ");
        }
        let x = f64::from(i % 17) / 16.0;
        let y = f64::from(i % 13) / 12.0;
        let z = f64::from(i % 11) / 10.0;
        let w = f64::from(i % 7) / 6.0;
        let _ = write!(values, "({i}, 'cat_{}', ARRAY[{x}, {y}, {z}, {w}])", i % 8);
    }
    engine
        .sql(&values, &[])
        .expect("insert operator join corpus");
    engine
        .sql("ANALYZE operator_docs", &[])
        .expect("analyze operator join corpus");
    engine
        .sql(
            "CREATE TABLE operator_groups (id INTEGER PRIMARY KEY, category TEXT)",
            &[],
        )
        .expect("create operator join groups");
    let mut groups = String::from("INSERT INTO operator_groups (id, category) VALUES ");
    for i in 0_u32..32 {
        if i > 0 {
            groups.push_str(", ");
        }
        let _ = write!(groups, "({i}, 'cat_{}')", i % 8);
    }
    engine
        .sql(&groups, &[])
        .expect("insert operator join groups");
    engine
        .sql("ANALYZE operator_groups", &[])
        .expect("analyze operator join groups");
    engine
}

fn bench_oltp(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "e2e_oltp_point_lookup",
            "SELECT * FROM bench WHERE id = 500",
        ),
        (
            "e2e_oltp_range_scan",
            "SELECT * FROM bench WHERE value BETWEEN 100 AND 200",
        ),
        (
            "e2e_oltp_update_where",
            "UPDATE bench SET value = value + 1 WHERE id = 500",
        ),
    ];
    let mut group = c.benchmark_group("e2e_oltp");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("query");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();

    let next_insert_id = AtomicI64::new(100_000);
    c.bench_function("e2e_oltp_insert_single", |bencher| {
        bencher.iter(|| {
            let next_id = next_insert_id.fetch_add(1, Ordering::Relaxed);
            let sql = format!("INSERT INTO audit (id, value) VALUES ({next_id}, 1)");
            let result = engine.sql(black_box(&sql), &[]).expect("insert");
            black_box(result.rows.len())
        });
    });
    let next_delete_id = AtomicI64::new(200_000);
    c.bench_function("e2e_oltp_delete_where", |bencher| {
        bencher.iter(|| {
            let next_id = next_delete_id.fetch_add(1, Ordering::Relaxed);
            let insert = format!("INSERT INTO audit (id, value) VALUES ({next_id}, 1)");
            let delete = format!("DELETE FROM audit WHERE id = {next_id}");
            engine.sql(&insert, &[]).expect("insert before delete");
            let result = engine.sql(black_box(&delete), &[]).expect("delete");
            black_box(result.rows.len())
        });
    });
}

fn bench_olap(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "e2e_olap_aggregate_group",
            "SELECT category, COUNT(*), SUM(value) FROM bench GROUP BY category",
        ),
        (
            "e2e_olap_aggregate_having",
            "SELECT category, COUNT(*) AS c FROM bench GROUP BY category HAVING COUNT(*) > 10",
        ),
        (
            "e2e_olap_order_by_limit",
            "SELECT * FROM bench ORDER BY value DESC LIMIT 25",
        ),
        ("e2e_olap_distinct", "SELECT DISTINCT category FROM bench"),
    ];
    let mut group = c.benchmark_group("e2e_olap");
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

fn bench_joins(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "e2e_join_2way",
            "SELECT b.id, d.label FROM bench b JOIN dim d ON b.category = d.category",
        ),
        (
            "e2e_join_3way",
            "SELECT b.id, d1.label, d2.label FROM bench b JOIN dim d1 ON b.category = d1.category JOIN dim d2 ON d1.id = d2.id",
        ),
        (
            "e2e_join_with_filter",
            "SELECT b.id, d.label FROM bench b JOIN dim d ON b.category = d.category WHERE b.value > 500",
        ),
        (
            "e2e_join_with_aggregate",
            "SELECT d.label, COUNT(*) FROM bench b JOIN dim d ON b.category = d.category GROUP BY d.label",
        ),
    ];
    let mut group = c.benchmark_group("e2e_join");
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

fn bench_subquery_cte_window_analyze(c: &mut Criterion) {
    let engine = build_engine();
    let cases = [
        (
            "e2e_subquery_scalar",
            "SELECT id FROM bench WHERE value > (SELECT AVG(value) FROM bench)",
        ),
        (
            "e2e_subquery_exists",
            "SELECT id FROM bench b WHERE EXISTS (SELECT 1 FROM dim d WHERE d.category = b.category)",
        ),
        (
            "e2e_cte_single",
            "WITH filtered AS (SELECT * FROM bench WHERE value > 500) SELECT COUNT(*) FROM filtered",
        ),
        (
            "e2e_cte_multi",
            "WITH a AS (SELECT * FROM bench WHERE value > 500), b AS (SELECT * FROM a WHERE category = 'cat_1') SELECT COUNT(*) FROM b",
        ),
        (
            "e2e_window_row_number",
            "SELECT id, ROW_NUMBER() OVER (ORDER BY value DESC) AS rn FROM bench",
        ),
        (
            "e2e_window_rank_partitioned",
            "SELECT id, RANK() OVER (PARTITION BY category ORDER BY value DESC) AS rk FROM bench",
        ),
        ("e2e_analyze", "ANALYZE bench"),
    ];
    let mut group = c.benchmark_group("e2e_subquery_cte_window_analyze");
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

fn bench_planner_statistics(c: &mut Criterion) {
    let analyze_engine = build_planner_engine();
    c.bench_function("planner_histogram_analyze", |bencher| {
        bencher.iter(|| {
            let result = analyze_engine
                .sql(black_box("ANALYZE bench"), &[])
                .expect("analyze");
            black_box(result.rows.len())
        });
    });

    let engine = build_planner_engine();
    engine.sql("ANALYZE bench", &[]).expect("analyze setup");
    let cases = [
        (
            "planner_selectivity_equality",
            "SELECT * FROM bench WHERE category = 'cat_5'",
        ),
        (
            "planner_selectivity_range",
            "SELECT * FROM bench WHERE value BETWEEN 100 AND 500",
        ),
    ];
    let mut group = c.benchmark_group("planner_selectivity");
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

fn bench_operator_joins(c: &mut Criterion) {
    let engine = build_operator_join_engine();
    let cases = [
        (
            "e2e_operator_vector_similarity_join",
            "SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 'operator_docs',\
                 knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32),\
                 knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32),\
                 0.8\
             )",
        ),
        (
            "e2e_operator_hybrid_join",
            "SELECT left_doc_id, right_doc_id \
             FROM hybrid_join(\
                 'operator_docs',\
                 category = 'cat_1' AND \
                     knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32),\
                 category = 'cat_1' AND \
                     knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32)\
             )",
        ),
        (
            "e2e_operator_join_dpccp",
            "SELECT pairs.left_doc_id, docs.category \
             FROM vector_similarity_join(\
                 'operator_docs',\
                 knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32),\
                 knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32),\
                 0.8\
             ) AS pairs \
             JOIN operator_docs AS docs ON docs.id = pairs.left_doc_id",
        ),
        (
            "e2e_knn_local_access_dpccp",
            "SELECT docs.id, groups.id \
             FROM operator_docs AS docs \
             JOIN operator_groups AS groups ON groups.category = docs.category \
             WHERE knn_match(\
                 docs.embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 32\
             )",
        ),
    ];
    let mut group = c.benchmark_group("e2e_operator_join");
    for (name, sql) in cases {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = engine.sql(black_box(sql), &[]).expect("operator join");
                black_box(result.rows.len())
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_oltp,
    bench_olap,
    bench_joins,
    bench_subquery_cte_window_analyze,
    bench_planner_statistics,
    bench_operator_joins
);
criterion_main!(benches);
