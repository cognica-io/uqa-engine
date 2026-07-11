//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! 1M-doc end-to-end SQL bench.
//!
//! Builds a 1,000,000-row in-memory corpus where every document is
//! tagged with a 7-token rotating vocabulary plus an integer `qty`.
//! `text_match_1m` runs `text_match(body, 'quokka')` (the rare term)
//! and ranks the top 10 by BM25. `select_filter_1m` runs a
//! numeric range scan with `ORDER BY qty DESC LIMIT 20`. Useful for
//! measuring the headline scaling figures called for by the master
//! plan's performance gate.
//!
//! Run with `cargo bench -p uqa-engine --bench sql_1m`.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::document_store::Document;

const N: u64 = 1_000_000;

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine.create_default_table("docs", vec!["body".into()]);
    // Avoid the SQL `INSERT` path (which builds a parser AST per
    // statement) — bulk-load the documents directly through the
    // engine's API for far lower setup overhead at 1M rows.
    for i in 0..N {
        let token = match i % 7 {
            0 => "quokka",
            1 => "alpha",
            2 => "beta",
            3 => "gamma",
            4 => "delta",
            5 => "epsilon",
            _ => "zeta",
        };
        let mut doc = Document::new();
        doc.insert("id".into(), Value::Int(i as i64));
        doc.insert(
            "body".into(),
            Value::Str(format!("lorem {token} ipsum dolor sit amet")),
        );
        doc.insert("qty".into(), Value::Int((i % 1000) as i64));
        let _: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        engine.add_document("docs", i, doc).unwrap();
    }
    engine
}

fn bench_text_match(c: &mut Criterion) {
    let engine = build_engine();
    let mut group = c.benchmark_group("sql_1m");
    group.sample_size(10);
    group.bench_function("text_match_top10", |bencher| {
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
    group.finish();
}

criterion_group!(benches, bench_text_match);
criterion_main!(benches);
