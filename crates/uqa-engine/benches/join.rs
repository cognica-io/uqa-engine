//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL JOIN benchmark.
//!
//! `inner_join_10k_x_1k` measures the engine's INNER JOIN path
//! across a 10k-row `employees` table and a 1k-row `departments`
//! table. The query joins on `employees.dept_id = departments.id`
//! and aggregates by department; the cost is dominated by the
//! cross-product probing the engine does between the two tables.
//!
//! Run with `cargo bench -p uqa-engine --bench join`.

use std::fmt::Write as _;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_engine::Engine;

const EMPLOYEES: u64 = 10_000;
const DEPARTMENTS: u64 = 1_000;

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, dept_id INTEGER, salary INTEGER)",
            &[],
        )
        .expect("create employees");
    engine
        .sql(
            "CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT)",
            &[],
        )
        .expect("create departments");

    let mut emp = String::with_capacity(64 * EMPLOYEES as usize);
    emp.push_str("INSERT INTO employees (id, name, dept_id, salary) VALUES ");
    for i in 0..EMPLOYEES {
        if i > 0 {
            emp.push_str(", ");
        }
        let _ = write!(
            emp,
            "({i}, 'emp{i}', {}, {})",
            i % DEPARTMENTS,
            50_000 + (i % 50_000)
        );
    }
    engine.sql(&emp, &[]).expect("insert employees");

    let mut dep = String::with_capacity(48 * DEPARTMENTS as usize);
    dep.push_str("INSERT INTO departments (id, name) VALUES ");
    for i in 0..DEPARTMENTS {
        if i > 0 {
            dep.push_str(", ");
        }
        let _ = write!(dep, "({i}, 'dept{i}')");
    }
    engine.sql(&dep, &[]).expect("insert departments");
    engine
}

fn bench_inner_join(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("sql_inner_join_10k_x_1k", |bencher| {
        bencher.iter(|| {
            let r = engine
                .sql(
                    "SELECT count(*) AS n \
                     FROM employees AS e \
                     INNER JOIN departments AS d ON e.dept_id = d.id \
                     WHERE e.salary > 70000",
                    &[],
                )
                .expect("ok");
            black_box(r.rows.len())
        });
    });
}

criterion_group!(benches, bench_inner_join);
criterion_main!(benches);
