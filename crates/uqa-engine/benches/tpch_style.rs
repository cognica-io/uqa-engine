//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! TPC-H-style analytical SQL benchmarks.
//!
//! The data is synthetic and intentionally compact enough for a local
//! benchmark loop, while preserving the execution shapes that matter:
//! Q1-style low-cardinality grouping with several streaming aggregates
//! and Q6-style conjunctive range predicates lowered through the shared
//! posting-list execution path.
//!
//! Run with `cargo bench -p uqa-engine --bench tpch_style`.

use std::fmt::Write as _;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::Value;
use uqa_engine::Engine;

const LINEITEMS: u64 = 100_000;
const FLAGS: [&str; 3] = ["A", "N", "R"];
const STATUSES: [&str; 2] = ["F", "O"];

const Q1: &str = "\
    SELECT return_flag, line_status, \
           SUM(quantity) AS sum_qty, \
           SUM(extended_price) AS sum_base_price, \
           SUM(extended_price * (100 - discount) / 100) AS sum_disc_price, \
           SUM(extended_price * (100 - discount) * (100 + tax) / 10000) AS sum_charge, \
           AVG(quantity) AS avg_qty, \
           AVG(extended_price) AS avg_price, \
           AVG(discount) AS avg_disc, \
           COUNT(*) AS count_order \
      FROM lineitem \
     WHERE ship_day <= 2449 \
     GROUP BY return_flag, line_status \
     ORDER BY return_flag, line_status";

const Q6: &str = "\
    SELECT SUM(extended_price * discount) AS revenue \
      FROM lineitem \
     WHERE ship_day BETWEEN 365 AND 2190 \
       AND discount BETWEEN 2 AND 8 \
       AND quantity < 40";

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE lineitem (\
                id INTEGER PRIMARY KEY, \
                return_flag TEXT, \
                line_status TEXT, \
                quantity INTEGER, \
                extended_price INTEGER, \
                discount INTEGER, \
                tax INTEGER, \
                ship_day INTEGER\
            )",
            &[],
        )
        .expect("create lineitem");

    // These scalar indexes all produce the same PostingList abstraction;
    // Q6 therefore measures conjunction across differently distributed
    // physical values without introducing a second filtering representation.
    for sql in [
        "CREATE INDEX lineitem_ship_day_idx ON lineitem (ship_day)",
        "CREATE INDEX lineitem_discount_idx ON lineitem (discount)",
        "CREATE INDEX lineitem_quantity_idx ON lineitem (quantity)",
    ] {
        engine.sql(sql, &[]).expect("create scalar index");
    }

    let mut insert = String::with_capacity(LINEITEMS as usize * 80);
    insert.push_str(
        "INSERT INTO lineitem \
         (id, return_flag, line_status, quantity, extended_price, discount, tax, ship_day) \
         VALUES ",
    );
    for id in 0..LINEITEMS {
        if id > 0 {
            insert.push_str(", ");
        }
        let _ = write!(
            insert,
            "({id}, '{}', '{}', {}, {}, {}, {}, {})",
            FLAGS[id as usize % FLAGS.len()],
            STATUSES[(id as usize / FLAGS.len()) % STATUSES.len()],
            1 + id % 50,
            10_000 + id % 90_000,
            id % 11,
            id % 9,
            id % 2_500,
        );
    }
    engine.sql(&insert, &[]).expect("insert lineitem");

    let q1 = engine.sql(Q1, &[]).expect("Q1 smoke");
    assert_eq!(q1.rows.len(), 6);
    let q1_count: i64 = q1
        .rows
        .iter()
        .map(|row| match row.get("count_order") {
            Some(Value::Int(count)) => *count,
            other => panic!("Q1 count_order must be an integer, got {other:?}"),
        })
        .sum();
    let expected_q1_count = (0..LINEITEMS).filter(|id| id % 2_500 <= 2_449).count() as i64;
    assert_eq!(q1_count, expected_q1_count);

    let q6 = engine.sql(Q6, &[]).expect("Q6 smoke");
    assert_eq!(q6.rows.len(), 1);
    let expected_q6_revenue: i64 = (0..LINEITEMS)
        .filter(|id| {
            let ship_day = id % 2_500;
            let discount = id % 11;
            let quantity = 1 + id % 50;
            (365..=2_190).contains(&ship_day) && (2..=8).contains(&discount) && quantity < 40
        })
        .map(|id| ((10_000 + id % 90_000) * (id % 11)) as i64)
        .sum();
    assert_eq!(
        q6.rows[0].get("revenue"),
        Some(&Value::Int(expected_q6_revenue))
    );
    engine
}

fn bench_tpch_style(c: &mut Criterion) {
    let engine = build_engine();
    let mut group = c.benchmark_group("tpch_style");
    group.sample_size(20);

    group.bench_function("q1_100k", |bencher| {
        bencher.iter(|| {
            let result = engine.sql(black_box(Q1), &[]).expect("Q1");
            black_box(result.rows)
        });
    });
    group.bench_function("q6_100k", |bencher| {
        bencher.iter(|| {
            let result = engine.sql(black_box(Q6), &[]).expect("Q6");
            black_box(result.rows)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_tpch_style);
criterion_main!(benches);
