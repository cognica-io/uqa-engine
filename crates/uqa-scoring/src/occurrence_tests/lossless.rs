//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact query identities and live cursor boundaries in every WAND implementation.

use super::*;
use uqa_analysis::TokenTerm;
use uqa_storage::{inverted_index::analyze_query_terms, MemoryInvertedIndex};

#[test]
fn raw_query_terms_repeat_without_aliases_and_maximum_document_ids_remain_live() {
    let Some(index) = raw_fixture() else {
        return;
    };
    let raw = TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd83d]));
    let replacement = TokenTermKey::from_text("�");
    let query_terms = vec![raw.clone(), replacement, raw.clone()];
    assert_eq!(index.doc_freq_key("body", &raw).unwrap(), 3);
    assert_eq!(index.doc_freq("body", "�").unwrap(), 1);
    let revision = index.search_analyzer_revision("body").unwrap();
    let analyzed = analyze_query_terms(&revision, "🙂a 🙂a").unwrap();
    assert_eq!(analyzed.iter().filter(|term| **term == raw).count(), 2);
    let scorer = Arc::new(BM25Scorer::new(
        BM25Params::default(),
        Arc::new(index.field_stats("body").unwrap()),
    ));
    let expected: BTreeMap<_, _> = [
        (0, 1, 0, 2),
        (128, 0, 1, 1),
        (65_535, 2, 0, 5),
        (u64::MAX, 3, 0, 8),
    ]
    .into_iter()
    .map(|(id, raw_tf, replacement_tf, length)| {
        (
            id,
            2.0 * scorer.score(raw_tf, length, 3) + scorer.score(replacement_tf, length, 1),
        )
    })
    .collect();
    let mut bounds = BlockMaxIndex::new(1).unwrap();
    bounds
        .set_block_maxes_key(
            "docs",
            "body",
            &raw,
            [1, 2, 3].map(|tf| scorer.score(tf, tf * 3 - 1, 3)).to_vec(),
        )
        .unwrap();
    bounds
        .set_block_maxes("docs", "body", "�", vec![scorer.score(1, 1, 1)])
        .unwrap();
    for k in [0, 1, 3, 4] {
        let materialized = WANDQuery::new_keys(
            index
                .get_posting_lists_keys_bulk("body", &query_terms)
                .unwrap(),
            vec![scorer.clone(); 3],
            vec!["body".into(); 3],
            query_terms.clone(),
            k,
        )
        .unwrap();
        let cursors = CursorWANDQuery::new_keys(
            index
                .posting_cursors_keys_bulk("body", &query_terms)
                .unwrap(),
            vec![scorer.clone(); 3],
            vec!["body".into(); 3],
            query_terms.clone(),
            k,
        )
        .unwrap();
        let results = [
            WANDScorer::new(&materialized, Some(&index))
                .score_top_k()
                .unwrap(),
            BlockMaxWANDScorer::new(&materialized, Some(&index), &bounds, "docs")
                .score_top_k()
                .unwrap(),
            CursorWANDScorer::new(&cursors).score_top_k().unwrap(),
            CursorBlockMaxWANDScorer::new(&cursors, &bounds, "docs")
                .score_top_k()
                .unwrap(),
        ];
        let mut wanted = expected.iter().collect::<Vec<_>>();
        wanted.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let mut wanted_ids = wanted
            .into_iter()
            .take(k)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        wanted_ids.sort_unstable();
        assert_native_ranking(&index, &query_terms, k, &expected, &wanted_ids);
        for result in results {
            assert_eq!(result.top_k.doc_ids().collect::<Vec<_>>(), wanted_ids);
            for entry in &result.top_k {
                assert!(
                    (entry.payload.score - expected[&entry.doc_id]).abs() < 1e-12,
                    "doc {}: {} != {}",
                    entry.doc_id,
                    entry.payload.score,
                    expected[&entry.doc_id]
                );
            }
        }
    }
}

#[test]
fn scalar_wand_exhaustion_does_not_consume_the_largest_document_id() {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .try_add_documents(
            [(0, "a"), (1, "b"), (u64::MAX, "a a b")]
                .into_iter()
                .map(|(id, text)| (id, BTreeMap::from([("body".into(), text.into())])))
                .collect(),
        )
        .unwrap();
    let scorer = Arc::new(BM25Scorer::new(
        BM25Params::default(),
        Arc::new(index.field_stats("body").unwrap()),
    ));
    let terms = vec!["a".to_string(), "b".to_string(), "absent".to_string()];
    let materialized = WANDQuery::new(
        index.get_posting_lists_bulk("body", &terms).unwrap(),
        vec![scorer.clone(); 3],
        vec!["body".into(); 3],
        terms.clone(),
        3,
    )
    .unwrap();
    let cursors = CursorWANDQuery::new(
        index.posting_cursors_bulk("body", &terms).unwrap(),
        vec![scorer; 3],
        vec!["body".into(); 3],
        terms,
        3,
    )
    .unwrap();
    let bounds = BlockMaxIndex::default();
    for result in [
        WANDScorer::new(&materialized, Some(&index))
            .score_top_k()
            .unwrap(),
        BlockMaxWANDScorer::new(&materialized, Some(&index), &bounds, "docs")
            .score_top_k()
            .unwrap(),
        CursorWANDScorer::new(&cursors).score_top_k().unwrap(),
        CursorBlockMaxWANDScorer::new(&cursors, &bounds, "docs")
            .score_top_k()
            .unwrap(),
    ] {
        assert_eq!(result.top_k.doc_ids().collect::<Vec<_>>(), [0, 1, u64::MAX]);
    }
}

fn raw_fixture() -> Option<MemoryInvertedIndex> {
    let config: Analyzer = match serde_json::from_str(
        r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","discard_punctuation":false,"user_dictionary":"🙂a 가 나"},"token_filters":[]}"#,
    ) {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return None;
        }
    };
    let mut index = MemoryInvertedIndex::new(config);
    let sources = [
        (0, "🙂a"),
        (128, "�"),
        (65_535, "🙂a 🙂a"),
        (u64::MAX, "🙂a 🙂a 🙂a"),
    ];
    index
        .try_add_documents(
            sources
                .into_iter()
                .map(|(id, text)| (id, BTreeMap::from([("body".into(), text.into())])))
                .collect(),
        )
        .unwrap();
    Some(index)
}

fn assert_native_ranking(
    index: &dyn InvertedIndex,
    terms: &[TokenTermKey],
    top_k: usize,
    expected: &BTreeMap<u64, f64>,
    wanted_ids: &[u64],
) {
    use crate::TextSearchAlgorithm::{BlockMaxWand, Exhaustive, Wand};
    for strategy in [Exhaustive, Wand, BlockMaxWand] {
        let profile = crate::score_text_terms(
            index,
            "docs",
            "body",
            terms,
            &crate::ScoringMode::BM25(BM25Params::default()),
            top_k,
            strategy,
        )
        .unwrap();
        let mut ids = profile
            .entries
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, wanted_ids);
        for entry in profile.entries {
            assert!((entry.score - expected[&entry.doc_id]).abs() < 1e-12);
        }
        assert_eq!(
            profile.algorithm,
            if strategy == BlockMaxWand {
                Wand
            } else {
                strategy
            }
        );
    }
}
