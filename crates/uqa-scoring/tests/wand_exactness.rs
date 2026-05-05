//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND / BMW exactness against exhaustive scoring + skip-rate gates
//! from the master plan: standard WAND skip-rate >= 60%, BMW >= 75% on
//! a representative corpus.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::analyzer::standard_analyzer;
use uqa_core::{DocId, FieldName, IndexStats};
use uqa_scoring::{BM25Params, BM25Scorer, BlockMaxWANDScorer, Scorer, WANDQuery, WANDScorer};
use uqa_storage::{BlockMaxIndex, InvertedIndex, MemoryInvertedIndex, DEFAULT_BLOCK_SIZE};

/// A 1000-doc corpus designed to exercise WAND/BMW pruning. Uses
/// non-stopword tokens so the Porter analyzer keeps every term, and
/// sets up a clear gradient: a very common term ("crate") in 90% of
/// docs, a rare term ("plan") in ~5%, and a mid-frequency term
/// ("rust") in ~33%.
fn build_corpus() -> Vec<(DocId, String)> {
    const N: u64 = 1000;
    let mut docs = Vec::with_capacity(N as usize);
    for i in 1..=N {
        let mut tokens: Vec<&'static str> = Vec::new();
        // High-df, low-IDF noise term (kept by analyzer; not a stopword).
        if i % 10 != 0 {
            tokens.push("crate");
        }
        // Mid-frequency: every 3rd doc.
        if i % 3 == 0 {
            tokens.extend(["rust", "rust"]);
        }
        // Low-frequency: every 20th doc, with high tf so when present
        // the score is dominant.
        if i % 20 == 0 {
            tokens.extend(std::iter::repeat_n("plan", 5));
        }
        let extras = (i % 7) as usize;
        tokens.extend(std::iter::repeat_n("filler", extras));
        docs.push((i, tokens.join(" ")));
    }
    docs
}

fn build_index(docs: &[(DocId, String)]) -> MemoryInvertedIndex {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    for (doc_id, body) in docs {
        let mut fields: BTreeMap<FieldName, String> = BTreeMap::new();
        fields.insert("body".into(), body.clone());
        idx.add_document(*doc_id, fields);
    }
    idx
}

fn exhaustive_top_k(
    idx: &dyn InvertedIndex,
    scorers: &[Arc<dyn Scorer>],
    fields: &[FieldName],
    terms: &[String],
    k: usize,
) -> Vec<(DocId, f64)> {
    let mut all_ids: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::default();
    for term in terms {
        for entry in &idx.get_posting_list("body", term) {
            all_ids.insert(entry.doc_id);
        }
    }
    let mut scored: Vec<(DocId, f64)> = Vec::new();
    for doc_id in all_ids {
        let mut total = 0.0;
        for (i, term) in terms.iter().enumerate() {
            let tf = idx.get_term_freq(doc_id, &fields[i], term);
            if tf == 0 {
                continue;
            }
            let df = idx.doc_freq(&fields[i], term);
            let dl = idx.get_doc_length(doc_id, &fields[i]).max(tf);
            total += scorers[i].score(tf, dl, df);
        }
        if total > 0.0 {
            scored.push((doc_id, total));
        }
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored.truncate(k);
    scored
}

fn bm25_scorer(stats: Arc<IndexStats>) -> Arc<dyn Scorer> {
    Arc::new(BM25Scorer::new(BM25Params::default(), stats)) as Arc<dyn Scorer>
}

fn assert_top_k_matches(got: &[(DocId, f64)], expected: &[(DocId, f64)]) {
    assert_eq!(got.len(), expected.len(), "top-k length mismatch");
    for (a, b) in got.iter().zip(expected) {
        assert_eq!(a.0, b.0, "doc_id order mismatch");
        assert!(
            (a.1 - b.1).abs() < 1e-9,
            "score delta {} too large at doc {}",
            (a.1 - b.1).abs(),
            a.0,
        );
    }
}

#[test]
fn wand_top_k_matches_exhaustive_and_skips_at_least_60pct() {
    let docs = build_corpus();
    let idx = build_index(&docs);
    let stats = Arc::new(idx.stats());

    // Multi-term query: rare ("language") + mid ("rust") + frequent ("the").
    let terms = ["plan", "rust", "crate"]
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();
    let posting_lists = terms
        .iter()
        .map(|t| idx.get_posting_list("body", t))
        .collect::<Vec<_>>();
    let scorers = (0..terms.len())
        .map(|_| bm25_scorer(stats.clone()))
        .collect::<Vec<_>>();
    let fields = vec![FieldName::from("body"); terms.len()];

    let q = WANDQuery::new(
        posting_lists.clone(),
        scorers.clone(),
        fields.clone(),
        terms.clone(),
        10,
    );
    let result = WANDScorer::new(&q, Some(&idx)).score_top_k();
    let mut got: Vec<(DocId, f64)> = result
        .top_k
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    got.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let expected = exhaustive_top_k(&idx, &scorers, &fields, &terms, 10);
    assert_top_k_matches(&got, &expected);

    let skip_rate = result.stats.skip_rate();
    assert!(
        skip_rate >= 0.60,
        "WAND skip rate {} below 60% (scored={} of {})",
        skip_rate,
        result.stats.scored,
        result.stats.total_candidates,
    );
}

#[test]
fn bmw_top_k_matches_exhaustive_and_skips_at_least_75pct() {
    let docs = build_corpus();
    let idx = build_index(&docs);
    let stats = Arc::new(idx.stats());

    let terms = ["plan", "rust", "crate"]
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();
    let posting_lists = terms
        .iter()
        .map(|t| idx.get_posting_list("body", t))
        .collect::<Vec<_>>();
    let scorers = (0..terms.len())
        .map(|_| bm25_scorer(stats.clone()))
        .collect::<Vec<_>>();
    let fields = vec![FieldName::from("body"); terms.len()];

    // Build BlockMaxIndex over each posting list.
    let mut bmi = BlockMaxIndex::new(DEFAULT_BLOCK_SIZE);
    for (i, term) in terms.iter().enumerate() {
        let bm25 = BM25Scorer::new(BM25Params::default(), stats.clone());
        bmi.build(&posting_lists[i], &bm25, "body", term, "articles");
    }

    let q = WANDQuery::new(
        posting_lists,
        scorers.clone(),
        fields.clone(),
        terms.clone(),
        10,
    );
    let result = BlockMaxWANDScorer::new(&q, Some(&idx), &bmi, "articles").score_top_k();
    let mut got: Vec<(DocId, f64)> = result
        .top_k
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    got.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let expected = exhaustive_top_k(&idx, &scorers, &fields, &terms, 10);
    assert_top_k_matches(&got, &expected);

    let skip_rate = result.stats.skip_rate();
    assert!(
        skip_rate >= 0.75,
        "BMW skip rate {} below 75% (scored={} of {})",
        skip_rate,
        result.stats.scored,
        result.stats.total_candidates,
    );
}

#[test]
fn bmw_skip_rate_meets_or_exceeds_wand() {
    // BMW has tighter bounds than WAND on the same corpus, so its skip
    // rate is at least as high. This is the key correctness contract
    // from Theorem 6.2.x in Paper 3.
    let docs = build_corpus();
    let idx = build_index(&docs);
    let stats = Arc::new(idx.stats());

    let terms = ["rust", "crate"]
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();
    let posting_lists = terms
        .iter()
        .map(|t| idx.get_posting_list("body", t))
        .collect::<Vec<_>>();
    let scorers = (0..terms.len())
        .map(|_| bm25_scorer(stats.clone()))
        .collect::<Vec<_>>();
    let fields = vec![FieldName::from("body"); terms.len()];

    let mut bmi = BlockMaxIndex::new(DEFAULT_BLOCK_SIZE);
    for (i, term) in terms.iter().enumerate() {
        let bm25 = BM25Scorer::new(BM25Params::default(), stats.clone());
        bmi.build(&posting_lists[i], &bm25, "body", term, "articles");
    }

    let q = WANDQuery::new(
        posting_lists,
        scorers.clone(),
        fields.clone(),
        terms.clone(),
        5,
    );
    let wand_stats = WANDScorer::new(&q, Some(&idx)).score_top_k().stats;
    let bmw_stats = BlockMaxWANDScorer::new(&q, Some(&idx), &bmi, "articles")
        .score_top_k()
        .stats;
    assert!(
        bmw_stats.skip_rate() >= wand_stats.skip_rate() - 1e-9,
        "BMW skip {} should not be worse than WAND skip {}",
        bmw_stats.skip_rate(),
        wand_stats.skip_rate(),
    );
}
