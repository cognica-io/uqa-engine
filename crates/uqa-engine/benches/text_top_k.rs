//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode, TextSearchAlgorithm};
use uqa_scoring::BM25Params;
use uqa_storage::document_store::Document;

fn populate(engine: &Engine, count: u64) {
    engine
        .create_default_table("docs", vec!["body".into()])
        .unwrap();
    for doc_id in 1..=count {
        let mut tokens = Vec::new();
        if doc_id % 10 != 0 {
            tokens.push("crate");
        }
        if doc_id % 3 == 0 {
            tokens.extend(["rust", "rust"]);
        }
        if doc_id % 41 == 0 {
            tokens.extend(std::iter::repeat_n("plan", 8));
        }
        tokens.extend(std::iter::repeat_n("filler", (doc_id % 13) as usize));
        let mut document = Document::new();
        document.insert("body".into(), Value::Str(tokens.join(" ")));
        engine.add_document("docs", doc_id, document).unwrap();
    }
}

fn bench_text_top_k(c: &mut Criterion) {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("text-top-k.sqlite3");
    let engine = Engine::open(&database_path).unwrap();
    populate(&engine, 5_000);
    let bmw_mode = ScoringMode::BM25(BM25Params::default());
    engine
        .rebuild_text_block_max("docs", "body", &bmw_mode)
        .unwrap();
    let wand_mode = ScoringMode::BM25(BM25Params {
        k1: 1.6,
        ..BM25Params::default()
    });

    let query = "plan rust crate";
    let k = 10;
    let bmw_profile = engine
        .search_profiled("docs", "body", query, &bmw_mode, k)
        .unwrap();
    let wand_profile = engine
        .search_profiled("docs", "body", query, &wand_mode, k)
        .unwrap();
    assert_eq!(bmw_profile.algorithm, TextSearchAlgorithm::BlockMaxWand);
    assert_eq!(wand_profile.algorithm, TextSearchAlgorithm::Wand);
    eprintln!(
        "text_top_k_profile algorithm=BMW candidates={} scored={} skip_rate={:.6} warm_latency_ms={:.6}",
        bmw_profile.total_candidates,
        bmw_profile.scored_candidates,
        bmw_profile.skip_rate,
        bmw_profile.elapsed_ms,
    );
    eprintln!(
        "text_top_k_profile algorithm=WAND candidates={} scored={} skip_rate={:.6} warm_latency_ms={:.6}",
        wand_profile.total_candidates,
        wand_profile.scored_candidates,
        wand_profile.skip_rate,
        wand_profile.elapsed_ms,
    );

    let mut group = c.benchmark_group("engine_text_top_k");
    group.throughput(Throughput::Elements(bmw_profile.total_candidates));
    let bmw_label = format!(
        "candidates={}_scored={}_skip={:.3}",
        bmw_profile.total_candidates, bmw_profile.scored_candidates, bmw_profile.skip_rate
    );
    group.bench_with_input(
        BenchmarkId::new("block_max_wand", bmw_label),
        &bmw_mode,
        |benchmark, mode| {
            benchmark.iter(|| {
                black_box(
                    engine
                        .search_profiled("docs", "body", black_box(query), mode, k)
                        .unwrap(),
                )
            });
        },
    );
    let wand_label = format!(
        "candidates={}_scored={}_skip={:.3}",
        wand_profile.total_candidates, wand_profile.scored_candidates, wand_profile.skip_rate
    );
    group.bench_with_input(
        BenchmarkId::new("wand", wand_label),
        &wand_mode,
        |benchmark, mode| {
            benchmark.iter(|| {
                black_box(
                    engine
                        .search_profiled("docs", "body", black_box(query), mode, k)
                        .unwrap(),
                )
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_text_top_k);
criterion_main!(benches);
