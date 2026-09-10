//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` vector persistence round-trip benchmarks.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_storage::VectorIndex;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteVectorIndex};

fn vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 32) as u32 as f32) / (u32::MAX as f32)
        })
        .collect()
}

fn bench_vector_persistence(c: &mut Criterion) {
    c.bench_function("vector_index_persistence_roundtrip", |bencher| {
        bencher.iter(|| {
            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("vectors.db");
            {
                let conn = ManagedConnection::open(&path).expect("open sqlite");
                let _catalog = Catalog::open(conn.clone()).expect("initialize sqlite catalog");
                let mut idx = SQLiteVectorIndex::new(conn, "docs", "embedding", 8);
                for id in 0..128 {
                    idx.add(id, vector(id + 1, 8)).unwrap();
                }
            }
            let conn = ManagedConnection::open(&path).expect("reopen sqlite");
            let _catalog = Catalog::open(conn.clone()).expect("reopen sqlite catalog");
            let idx = SQLiteVectorIndex::new(conn, "docs", "embedding", 8);
            black_box(idx.count().unwrap())
        });
    });
}

criterion_group!(benches, bench_vector_persistence);
criterion_main!(benches);
