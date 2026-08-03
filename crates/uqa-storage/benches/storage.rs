//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document, inverted-index, and vector-index storage benchmarks.
//!
//! Covers document-store put/get/scan, inverted-index add/lookup,
//! brute-force, IVF, and HNSW vector search, vector deletion, index builds,
//! and `SQLite` vector persistence round trips.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_analysis::standard_analyzer;
use uqa_core::Value;
use uqa_storage::sqlite::{Catalog, ManagedConnection, SQLiteVectorIndex};
use uqa_storage::{
    DocumentStore, HNSWIndex, HNSWIndexParams, IVFIndex, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, MemoryVectorIndex, VectorIndex,
};

fn doc(id: u64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("id".to_string(), Value::Int(id as i64)),
        ("name".to_string(), Value::Str(format!("doc_{id}"))),
        (
            "body".to_string(),
            Value::Str(format!("term_{} common", id % 100)),
        ),
    ])
}

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

fn populated_docs(n: u64) -> MemoryDocumentStore {
    let mut store = MemoryDocumentStore::new();
    for id in 0..n {
        store.put(id, doc(id)).unwrap();
    }
    store
}

fn populated_inverted(n: u64) -> MemoryInvertedIndex {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    for id in 0..n {
        idx.add_document(
            id,
            BTreeMap::from([(
                "body".to_string(),
                format!("term_{:06} common topic_{}", id % 1_000, id % 10),
            )]),
        )
        .unwrap();
    }
    idx
}

fn populated_vector(n: u64, dim: usize) -> MemoryVectorIndex {
    let mut idx = MemoryVectorIndex::new(dim as u32);
    for id in 0..n {
        idx.add(id, vector(id + 1, dim)).unwrap();
    }
    idx
}

fn populated_ivf(n: u64, dim: usize) -> IVFIndex {
    let mut idx = IVFIndex::with_params(dim as u32, 16, 4, 256);
    for id in 0..n {
        idx.add(id, vector(id + 1, dim)).unwrap();
    }
    idx.train().expect("train IVF benchmark index");
    idx
}

fn populated_hnsw(n: u64, dim: usize) -> HNSWIndex {
    let mut index = HNSWIndex::with_params(
        dim as u32,
        HNSWIndexParams {
            m: 16,
            ef_construction: 64,
            ef_search: 64,
            rebuild_threshold: 1_024,
            seed: 7,
        },
    )
    .expect("valid HNSW benchmark parameters");
    for doc_id in 0..n {
        index.add(doc_id, vector(doc_id + 1, dim)).unwrap();
    }
    index
}

fn bench_document_store(c: &mut Criterion) {
    c.bench_function("document_store_put_single", |bencher| {
        let mut store = MemoryDocumentStore::new();
        let mut next_id = 0_u64;
        bencher.iter(|| {
            store.put(next_id, doc(next_id)).unwrap();
            next_id += 1;
            black_box(store.len().unwrap())
        });
    });

    let mut batch_group = c.benchmark_group("document_store_put_batch");
    for batch_size in [10_u64, 100] {
        batch_group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |bencher, batch_size| {
                bencher.iter(|| {
                    let mut store = MemoryDocumentStore::new();
                    for id in 0..*batch_size {
                        store.put(id, doc(id)).unwrap();
                    }
                    black_box(store.len().unwrap())
                });
            },
        );
    }
    batch_group.finish();

    let store = populated_docs(10_000);
    c.bench_function("document_store_get_random_10k", |bencher| {
        bencher.iter(|| {
            let mut found = 0_usize;
            for id in [17_u64, 2_003, 7_771, 9_999] {
                found += usize::from(store.get(black_box(id)).unwrap().is_some());
            }
            black_box(found)
        });
    });
    c.bench_function("document_store_scan_all_10k", |bencher| {
        bencher.iter(|| black_box(store.iter_all().unwrap().count()));
    });
}

fn bench_inverted_index(c: &mut Criterion) {
    c.bench_function("inverted_index_add_document", |bencher| {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        let mut next_id = 0_u64;
        bencher.iter(|| {
            idx.add_document(
                next_id,
                BTreeMap::from([(
                    "body".to_string(),
                    format!("term_{:06} common", next_id % 1_000),
                )]),
            )
            .unwrap();
            next_id += 1;
            black_box(idx.doc_count().unwrap())
        });
    });

    let idx = populated_inverted(10_000);
    c.bench_function("inverted_index_get_posting_list", |bencher| {
        bencher.iter(|| {
            let pl = idx
                .get_posting_list(black_box("body"), black_box("term_000001"))
                .unwrap();
            black_box(pl.len())
        });
    });
    c.bench_function("inverted_index_doc_freq", |bencher| {
        bencher.iter(|| {
            black_box(
                idx.doc_freq(black_box("body"), black_box("term_000001"))
                    .unwrap(),
            )
        });
    });
}

fn bench_vector_index(c: &mut Criterion) {
    c.bench_function("vector_index_build_500", |bencher| {
        bencher.iter(|| {
            let idx = populated_vector(500, 32);
            black_box(idx.count().unwrap())
        });
    });

    c.bench_function("vector_index_add_single", |bencher| {
        let mut idx = MemoryVectorIndex::new(32);
        let mut next_id = 0_u64;
        bencher.iter(|| {
            idx.add(next_id, vector(next_id + 1, 32)).unwrap();
            next_id += 1;
            black_box(idx.count().unwrap())
        });
    });

    let brute = populated_vector(10_000, 32);
    let trained = populated_ivf(10_000, 32);
    let hnsw = populated_hnsw(10_000, 32);
    let query = vector(1, 32);
    let mut brute_group = c.benchmark_group("vector_index_knn_brute_force");
    for k in [10_usize, 50] {
        brute_group.bench_with_input(BenchmarkId::from_parameter(k), &k, |bencher, k| {
            bencher.iter(|| {
                let result = brute.search_knn(black_box(&query), black_box(*k)).unwrap();
                black_box(result.len())
            });
        });
    }
    brute_group.finish();

    let mut trained_group = c.benchmark_group("vector_index_knn_trained");
    for k in [10_usize, 50] {
        trained_group.bench_with_input(BenchmarkId::from_parameter(k), &k, |bencher, k| {
            bencher.iter(|| {
                let result = trained
                    .search_knn(black_box(&query), black_box(*k))
                    .unwrap();
                black_box(result.len())
            });
        });
    }
    trained_group.finish();

    let mut hnsw_group = c.benchmark_group("vector_index_knn_hnsw");
    for k in [10_usize, 50] {
        hnsw_group.bench_with_input(BenchmarkId::from_parameter(k), &k, |bencher, k| {
            bencher.iter(|| {
                let result = hnsw.search_knn(black_box(&query), black_box(*k)).unwrap();
                black_box(result.len())
            });
        });
    }
    hnsw_group.finish();

    c.bench_function("vector_index_threshold_search", |bencher| {
        bencher.iter(|| {
            let result = brute
                .search_threshold(black_box(&query), black_box(0.85))
                .unwrap();
            black_box(result.len())
        });
    });

    c.bench_function("vector_index_delete", |bencher| {
        bencher.iter(|| {
            let mut idx = populated_vector(1_000, 32);
            idx.delete(500).unwrap();
            black_box(idx.count().unwrap())
        });
    });

    c.bench_function("vector_index_train", |bencher| {
        bencher.iter(|| {
            let mut idx = IVFIndex::with_params(32, 16, 4, 256);
            for id in 0..1_000 {
                idx.add(id, vector(id + 1, 32)).unwrap();
            }
            idx.train().expect("train IVF benchmark index");
            black_box(idx.count().unwrap())
        });
    });

    c.bench_function("vector_index_hnsw_build_1k", |bencher| {
        bencher.iter(|| black_box(populated_hnsw(1_000, 32).count().unwrap()));
    });
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

criterion_group!(
    benches,
    bench_document_store,
    bench_inverted_index,
    bench_vector_index,
    bench_vector_persistence
);
criterion_main!(benches);
