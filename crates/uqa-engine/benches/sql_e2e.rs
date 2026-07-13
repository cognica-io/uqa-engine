//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end SQL benchmarks.
//!
//! `text_match_10k` measures the full search pipeline (parse SQL,
//! tokenize the query, walk the inverted index, score with BM25, and
//! build the result rows) over a 10k-document in-memory corpus.
//! `select_filter_10k` covers a non-text path: a numeric range scan
//! with ORDER BY + LIMIT.
//!
//! Run with `cargo bench -p uqa-engine --bench sql_e2e`.

use std::fmt::Write as _;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_engine::Engine;

const N: u64 = 10_000;

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT, qty INTEGER)",
            &[],
        )
        .expect("create");
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .expect("create text index");
    let mut buffer = String::with_capacity(64 * N as usize);
    buffer.push_str("INSERT INTO docs (id, title, body, qty) VALUES ");
    for i in 0..N {
        if i > 0 {
            buffer.push_str(", ");
        }
        // Mix vocabulary so the inverted index sees realistic
        // distribution (every 7th doc gets the rare term `quokka`).
        let token = match i % 7 {
            0 => "quokka",
            1 => "alpha",
            2 => "beta",
            3 => "gamma",
            4 => "delta",
            5 => "epsilon",
            _ => "zeta",
        };
        let _ = write!(
            buffer,
            "({i}, 'doc {i}', 'lorem {token} ipsum dolor sit amet', {})",
            i % 1000
        );
    }
    engine.sql(&buffer, &[]).expect("insert");
    engine
}

fn bench_text_match(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("sql_text_match_10k", |bencher| {
        bencher.iter(|| {
            let r = engine
                .sql(
                    "SELECT id, _score FROM docs \
                     WHERE text_match(body, 'quokka') \
                     ORDER BY _score DESC LIMIT 10",
                    &[],
                )
                .expect("ok");
            black_box(r.rows.len())
        });
    });
}

fn bench_select_filter(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("sql_select_filter_10k", |bencher| {
        bencher.iter(|| {
            let r = engine
                .sql(
                    "SELECT id FROM docs WHERE qty > 800 ORDER BY qty DESC LIMIT 20",
                    &[],
                )
                .expect("ok");
            black_box(r.rows.len())
        });
    });
}

fn bench_multi_term_text_match(c: &mut Criterion) {
    let engine = build_engine();
    c.bench_function("sql_text_match_multi_term_10k", |bencher| {
        bencher.iter(|| {
            // Two-term query against the body field. The shared
            // tokenizer fans this into a multi-term posting walk -
            // i.e. WAND territory.
            let r = engine
                .sql(
                    "SELECT id, _score FROM docs \
                     WHERE text_match(body, 'quokka alpha') \
                     ORDER BY _score DESC LIMIT 10",
                    &[],
                )
                .expect("ok");
            black_box(r.rows.len())
        });
    });
}

criterion_group!(
    benches,
    bench_text_match,
    bench_select_filter,
    bench_multi_term_text_match
);
criterion_main!(benches);
