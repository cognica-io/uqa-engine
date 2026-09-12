//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise score-only readers with explicit occurrence frequency and normalization metadata.

use std::collections::BTreeMap;

use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources, TokenLengthPolicy};
use uqa_storage::clustered_postings::{encode_occurrence_cluster, OccurrencePosting};
use uqa_storage::inverted_index::analyze_index_field;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

use super::{Arc, BM25Scorer, Engine, InvertedIndex, ScoringMode};

fn occurrence_score_fixture() -> SQLiteInvertedIndex {
    let conn = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(conn.clone()).unwrap();
    let config: Analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a","b"]}}]}"#).unwrap();
    let compiled = AnalyzerResources::new(AnalyzerLimits::default())
        .compile_with_length_policy(&config, TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    let mut index = SQLiteInvertedIndex::new(conn.clone(), "docs", config);
    let mut terms = BTreeMap::<String, Vec<OccurrencePosting>>::new();
    let mut lengths = Vec::new();
    for (doc_id, source) in [(1, "a"), (2, "a a")] {
        index
            .add_document(doc_id, BTreeMap::from([("body".into(), source.into())]))
            .unwrap();
        let staged = analyze_index_field(&compiled, source).unwrap();
        lengths.push((doc_id, staged.length));
        for (key, occurrences) in staged.terms {
            terms
                .entry(key.to_term().into_string().unwrap())
                .or_default()
                .push(OccurrencePosting {
                    doc_id,
                    doc_length: staged.length,
                    occurrences,
                });
        }
    }
    conn.with_mut(|conn| {
        let tx = conn.savepoint()?;
        for (term, entries) in terms {
            let (score_blob, positions_blob) = encode_occurrence_cluster(&entries).unwrap();
            assert_eq!(tx.execute("UPDATE _posting_clusters SET score_blob = ?1, positions_blob = ?2 WHERE table_name = 'docs' AND field = 'body' AND term = ?3 AND cluster_id = 0", rusqlite::params![score_blob, positions_blob, term])?, 1);
        }
        for (doc_id, length) in &lengths {
            assert_eq!(tx.execute("UPDATE _doc_lengths SET length = ?1 WHERE table_name = 'docs' AND field = 'body' AND doc_id = ?2", rusqlite::params![i64::try_from(*length).unwrap(), i64::try_from(*doc_id).unwrap()])?, 1);
        }
        assert_eq!(tx.execute("UPDATE _field_stats SET total_length = ?1 WHERE table_name = 'docs' AND field = 'body'", [i64::try_from(lengths.iter().map(|(_, length)| length).sum::<u64>()).unwrap()])?, 1);
        tx.commit()?;
        Ok(())
    }).unwrap();
    index
}

#[test]
fn occurrence_scores_keep_actual_lengths_in_exhaustive_search_and_persisted_bounds() {
    let index = occurrence_score_fixture();
    let mode = ScoringMode::BM25(crate::BM25Params::default());
    let scorer = BM25Scorer::new(
        crate::BM25Params::default(),
        Arc::new(index.field_stats("body").unwrap()),
    );
    let expected =
        [(1, 3, 1, 1), (2, 6, 2, 2)].map(|(doc_id, a_frequency, b_frequency, length)| {
            (
                doc_id,
                scorer.score(a_frequency, length, 2),
                scorer.score(b_frequency, length, 2),
            )
        });
    let single = Engine::score_single_text_term(&index, "body", &["a".into()], &mode).unwrap();
    let multiple = Engine::score_multiple_text_terms(
        &index,
        "body",
        &["a".into(), "b".into(), "a".into()],
        &mode,
    )
    .unwrap();
    assert_eq!(single.len(), expected.len());
    assert_eq!(multiple.len(), expected.len());
    for ((single, multiple), (doc_id, a_score, b_score)) in
        single.iter().zip(&multiple).zip(expected)
    {
        assert_eq!((single.doc_id, multiple.doc_id), (doc_id, doc_id));
        assert!((single.score - a_score).abs() < 1e-12);
        assert!((multiple.score - (2.0 * a_score + b_score)).abs() < 1e-12);
    }
    index
        .build_block_max_scores_versioned("body", "a", &scorer, "occurrence-test-scorer")
        .unwrap();
    let maxima = index
        .persisted_block_max_scores("body", "a", "occurrence-test-scorer")
        .unwrap()
        .unwrap();
    assert_eq!(
        maxima,
        [expected
            .iter()
            .map(|(_, score, _)| *score)
            .fold(0.0, f64::max)]
    );
}
