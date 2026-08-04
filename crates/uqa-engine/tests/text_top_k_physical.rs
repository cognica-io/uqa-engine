//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, ScoredEntry, ScoringMode, TextSearchAlgorithm};
use uqa_scoring::{BM25Params, BayesianBM25Params};
use uqa_storage::document_store::Document;

fn document(body: String, title: String) -> Document {
    let mut document = Document::new();
    document.insert("body".into(), Value::Str(body));
    document.insert("title".into(), Value::Str(title));
    document
}

fn populate(engine: &Engine, count: u64) {
    engine
        .create_default_table("docs", vec!["body".into(), "title".into()])
        .unwrap();
    for doc_id in 1..=count {
        let mut body = Vec::new();
        if !doc_id.is_multiple_of(10) {
            body.push("crate");
        }
        if doc_id.is_multiple_of(3) {
            body.extend(["rust", "rust"]);
        }
        if doc_id.is_multiple_of(37) {
            body.extend(std::iter::repeat_n("plan", 7));
        }
        body.extend(std::iter::repeat_n("filler", (doc_id % 11) as usize));

        let mut title = Vec::new();
        if doc_id.is_multiple_of(2) {
            title.push("engine");
        }
        if doc_id.is_multiple_of(7) {
            title.extend(["query", "query", "query"]);
        }
        title.extend(std::iter::repeat_n("heading", (doc_id % 5) as usize));
        engine
            .add_document("docs", doc_id, document(body.join(" "), title.join(" ")))
            .unwrap();
    }
}

fn assert_same(actual: &[ScoredEntry], expected: &[ScoredEntry]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.doc_id, expected.doc_id);
        assert!(
            (actual.score - expected.score).abs() < 1e-10,
            "score mismatch for doc {}: {} != {}",
            actual.doc_id,
            actual.score,
            expected.score
        );
    }
}

fn assert_top_k_matches_exhaustive(
    engine: &Engine,
    field: &str,
    query: &str,
    mode: &ScoringMode,
    k: usize,
    expected_algorithm: TextSearchAlgorithm,
) {
    let exhaustive = engine
        .search("docs", field, query, mode, usize::MAX)
        .unwrap();
    let expected = exhaustive.into_iter().take(k).collect::<Vec<_>>();
    let public = engine.search("docs", field, query, mode, k).unwrap();
    let profile = engine
        .search_profiled("docs", field, query, mode, k)
        .unwrap();
    assert_same(&public, &expected);
    assert_same(&profile.entries, &expected);
    assert_eq!(profile.algorithm, expected_algorithm);
    assert!(profile.scored_candidates <= profile.total_candidates);
    assert!((0.0..=1.0).contains(&profile.skip_rate));
    assert!(profile.elapsed_ms >= 0.0);
}

#[test]
fn randomized_public_wand_matches_exhaustive_for_duplicate_terms_and_bayesian_finalizer() {
    let engine = Engine::new();
    populate(&engine, 320);
    let bm25 = ScoringMode::BM25(BM25Params::default());
    let bayesian = ScoringMode::BayesianBM25(BayesianBM25Params {
        alpha: 1.7,
        beta: 1.1,
        base_rate: 0.08,
        ..BayesianBM25Params::default()
    });
    let terms = ["plan", "rust", "crate", "filler"];
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    for case in 0_usize..48 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let term_count = 2 + (state as usize % 4);
        let mut query = Vec::with_capacity(term_count);
        for _ in 0..term_count {
            state = state
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);
            query.push(terms[state as usize % terms.len()]);
        }
        let query = query.join(" ");
        let k = 1 + (state as usize % 20);
        let mode = if case.is_multiple_of(2) {
            &bm25
        } else {
            &bayesian
        };
        assert_top_k_matches_exhaustive(
            &engine,
            "body",
            &query,
            mode,
            k,
            TextSearchAlgorithm::Wand,
        );
    }

    // Explicit duplicate occurrence and a different field pin the two
    // contracts that are easiest to accidentally erase while preparing WAND.
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "plan plan rust crate",
        &bayesian,
        10,
        TextSearchAlgorithm::Wand,
    );
    assert_top_k_matches_exhaustive(
        &engine,
        "title",
        "engine query query",
        &bm25,
        8,
        TextSearchAlgorithm::Wand,
    );

    let profile = engine
        .search_profiled("docs", "body", "plan rust crate", &bm25, 10)
        .unwrap();
    assert!(profile.scored_candidates < profile.total_candidates);

    // SQL's score-limit access path must push the same physical plan into the
    // text leaf instead of materializing and sorting the complete carrier.
    let expected = engine
        .search("docs", "body", "plan rust crate", &bm25, 10)
        .unwrap();
    let sql = engine
        .sql(
            "SELECT _doc_id, _score FROM docs \
             WHERE text_match(body, 'plan rust crate') \
             ORDER BY _score DESC LIMIT 10",
            &[],
        )
        .unwrap();
    let sql_ids = sql
        .rows
        .iter()
        .map(|row| match row.get("_doc_id") {
            Some(Value::Int(id)) => u64::try_from(*id).unwrap(),
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sql_ids,
        expected
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn sqlite_bmw_survives_reopen_and_never_uses_stale_or_mismatched_bounds() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("top-k.sqlite3");
    let mode = ScoringMode::BM25(BM25Params::default());

    {
        let engine = Engine::open(&path).unwrap();
        populate(&engine, 420);
        assert!(engine
            .rebuild_text_block_max("docs", "body", &mode)
            .unwrap());
        assert_top_k_matches_exhaustive(
            &engine,
            "body",
            "plan rust crate",
            &mode,
            10,
            TextSearchAlgorithm::BlockMaxWand,
        );
    }

    let engine = Engine::open(&path).unwrap();
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "plan rust crate",
        &mode,
        10,
        TextSearchAlgorithm::BlockMaxWand,
    );

    let mismatched = ScoringMode::BM25(BM25Params {
        k1: 1.8,
        ..BM25Params::default()
    });
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "plan rust crate",
        &mismatched,
        10,
        TextSearchAlgorithm::Wand,
    );

    engine
        .add_document(
            "docs",
            421,
            document("plan plan plan rust crate".into(), "engine query".into()),
        )
        .unwrap();
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "plan rust crate",
        &mode,
        10,
        TextSearchAlgorithm::Wand,
    );

    assert!(engine
        .rebuild_text_block_max("docs", "body", &mode)
        .unwrap());
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "plan rust crate",
        &mode,
        10,
        TextSearchAlgorithm::BlockMaxWand,
    );
}
