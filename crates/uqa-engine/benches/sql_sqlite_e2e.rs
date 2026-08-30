//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end SQL benchmarks against the persistent `SQLite` backend.
//!
//! The in-memory benches (`sql_e2e`, `sql_workloads`) miss the costs
//! that dominate persistent deployments: storage round trips, JSON
//! field extraction, statement caching, and value-index acceleration.
//! Every workload here runs through `Engine::open` on a temp file.

use criterion::{criterion_group, criterion_main, Criterion};
use uqa_engine::Engine;

const ROWS: usize = 10_000;
const BATCH: usize = 500;

fn seeded_engine(dir: &tempfile::TempDir, with_indexes: bool) -> Engine {
    let path = dir.path().join("bench.db");
    let engine = Engine::open(&path).expect("open persistent engine");
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, owner TEXT, qty INTEGER, price REAL, note TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE owners (name TEXT PRIMARY KEY, region TEXT)",
            &[],
        )
        .unwrap();
    let owner_rows: Vec<String> = (0..50)
        .map(|i| format!("('owner{i}', 'region{}')", i % 5))
        .collect();
    engine
        .sql(
            &format!(
                "INSERT INTO owners (name, region) VALUES {}",
                owner_rows.join(", ")
            ),
            &[],
        )
        .unwrap();
    if with_indexes {
        engine
            .sql("CREATE INDEX items_qty ON items USING btree (qty)", &[])
            .unwrap();
        engine
            .sql("CREATE INDEX items_owner ON items USING btree (owner)", &[])
            .unwrap();
    }
    let mut n = 0;
    while n < ROWS {
        let mut values = Vec::with_capacity(BATCH);
        for _ in 0..BATCH {
            n += 1;
            values.push(format!(
                "({n}, 'owner{}', {}, {}.5, 'note text {n}')",
                n % 50,
                n % 1000,
                n
            ));
        }
        engine
            .sql(
                &format!(
                    "INSERT INTO items (id, owner, qty, price, note) VALUES {}",
                    values.join(", ")
                ),
                &[],
            )
            .unwrap();
    }
    engine
}

fn warm_indexes(engine: &Engine) {
    // Touch each indexable column once so lazy index builds happen
    // outside the measured section.
    for sql in [
        "SELECT id FROM items WHERE id = 1",
        "SELECT id FROM items WHERE qty = 1",
        "SELECT id FROM items WHERE owner = 'owner1'",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}

fn bench_sqlite_reads(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded_engine(&dir, true);
    warm_indexes(&engine);

    let mut group = c.benchmark_group("sqlite_e2e");
    group.sample_size(30);

    let mut k: i64 = 0;
    group.bench_function("count_star_10k", |b| {
        b.iter(|| engine.sql("SELECT count(*) FROM items", &[]).unwrap());
    });
    group.bench_function("pk_point_select_10k", |b| {
        b.iter(|| {
            k = (k % (ROWS as i64)) + 1;
            engine
                .sql(&format!("SELECT id FROM items WHERE id = {k}"), &[])
                .unwrap()
        });
    });
    group.bench_function("indexed_eq_filter_10k", |b| {
        b.iter(|| {
            k = (k + 1) % 1000;
            engine
                .sql(&format!("SELECT id FROM items WHERE qty = {k}"), &[])
                .unwrap()
        });
    });
    group.bench_function("indexed_filter_order_limit_10k", |b| {
        b.iter(|| {
            k = (k + 1) % 50;
            engine
                .sql(
                    &format!(
                        "SELECT id, qty FROM items WHERE owner = 'owner{k}'
                         ORDER BY qty DESC LIMIT 10"
                    ),
                    &[],
                )
                .unwrap()
        });
    });
    group.bench_function("order_limit_unindexed_10k", |b| {
        b.iter(|| {
            engine
                .sql("SELECT id FROM items ORDER BY price DESC LIMIT 10", &[])
                .unwrap()
        });
    });
    group.bench_function("group_by_10k", |b| {
        b.iter(|| {
            engine
                .sql(
                    "SELECT owner, sum(qty) AS total FROM items
                     GROUP BY owner ORDER BY total DESC LIMIT 5",
                    &[],
                )
                .unwrap()
        });
    });
    group.bench_function("filtered_join_10k_x_50", |b| {
        b.iter(|| {
            k = (k + 1) % 1000;
            engine
                .sql(
                    &format!(
                        "SELECT i.id, o.region FROM items i
                         JOIN owners o ON i.owner = o.name
                         WHERE i.qty = {k} LIMIT 5"
                    ),
                    &[],
                )
                .unwrap()
        });
    });
    group.finish();
}

fn bench_sqlite_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("sqlite_e2e_write");
    group.sample_size(20);

    group.bench_function("insert_batch_500", |b| {
        let dir = tempfile::tempdir().unwrap();
        let engine = seeded_engine(&dir, false);
        let mut n = ROWS as i64;
        b.iter(|| {
            let mut values = Vec::with_capacity(BATCH);
            for _ in 0..BATCH {
                n += 1;
                values.push(format!(
                    "({n}, 'owner{}', {}, {}.5, 'note text {n}')",
                    n % 50,
                    n % 1000,
                    n
                ));
            }
            engine
                .sql(
                    &format!(
                        "INSERT INTO items (id, owner, qty, price, note) VALUES {}",
                        values.join(", ")
                    ),
                    &[],
                )
                .unwrap()
        });
    });

    group.bench_function("point_update_indexed", |b| {
        let dir = tempfile::tempdir().unwrap();
        let engine = seeded_engine(&dir, true);
        warm_indexes(&engine);
        let mut k: i64 = 0;
        b.iter(|| {
            k = (k % (ROWS as i64)) + 1;
            engine
                .sql(
                    &format!("UPDATE items SET qty = qty + 1 WHERE id = {k}"),
                    &[],
                )
                .unwrap()
        });
    });

    group.finish();
}

fn bench_sqlite_sessions(c: &mut Criterion) {
    let plain_dir = tempfile::tempdir().unwrap();
    let plain_engine = seeded_engine(&plain_dir, true);
    let encrypted_dir = tempfile::tempdir().unwrap();
    let encrypted_engine = Engine::open_encrypted(
        &encrypted_dir.path().join("encrypted-bench.db"),
        "benchmark-database-key",
    )
    .unwrap();

    let mut group = c.benchmark_group("sqlite_e2e_session");
    group.sample_size(20);
    group.bench_function("new_session_select_one_10k", |b| {
        b.iter(|| {
            let session = plain_engine.new_session().unwrap();
            session.sql("SELECT 1", &[]).unwrap()
        });
    });
    group.sample_size(10);
    group.bench_function("new_encrypted_session_select_one", |b| {
        b.iter(|| {
            let session = encrypted_engine.new_session().unwrap();
            session.sql("SELECT 1", &[]).unwrap()
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_sqlite_reads,
    bench_sqlite_writes,
    bench_sqlite_sessions
);
criterion_main!(benches);
