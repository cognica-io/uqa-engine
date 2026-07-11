//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector KNN benchmark: cosine similarity top-10 over a 10k-vector
//! corpus.
//!
//! Run with `cargo bench -p uqa-engine --bench knn`.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::document_store::Document;

const N: u64 = 10_000;
const DIMS: usize = 32;

fn deterministic_vec(seed: u64) -> Vec<f32> {
    // A deterministic mash over a few primes so each vector lands in
    // a different region of the unit ball; we don't need realistic
    // distributions, just a varied set the cosine path can chew on.
    let mut out = Vec::with_capacity(DIMS);
    let mut x = seed
        .wrapping_mul(2_654_435_761)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    for _ in 0..DIMS {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // Map the 64-bit state into [-1, 1].
        let bits = (x >> 32) as u32;
        let f = (bits as f32 / u32::MAX as f32) * 2.0 - 1.0;
        out.push(f);
    }
    out
}

fn build_engine() -> Engine {
    let engine = Engine::new();
    engine.create_default_table("docs", Vec::new());
    engine.create_vector_field("docs", "emb", DIMS as u32);
    for i in 0..N {
        let mut doc = Document::new();
        doc.insert("id".into(), Value::Int(i as i64));
        let vec = deterministic_vec(i);
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        vectors.insert("emb".into(), vec);
        engine.add_document_with_vectors("docs", i, doc, vectors).unwrap();
    }
    engine
}

fn bench_knn(c: &mut Criterion) {
    let engine = build_engine();
    let query = deterministic_vec(N + 1);
    c.bench_function("knn_top10_10k_dim32", |bencher| {
        bencher.iter(|| {
            let hits = engine.knn_search("docs", "emb", query.clone(), 10);
            black_box(hits.len())
        });
    });
}

criterion_group!(benches, bench_knn);
criterion_main!(benches);
