//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese graph frequencies and overlap lengths feed every lexical scoring path.

use std::sync::Arc;
use uqa_scoring::{
    score_text_terms, BM25Params, BM25Scorer, BlockMaxWANDScorer, CursorBlockMaxWANDScorer,
    CursorWANDQuery, CursorWANDScorer, ScoringMode, TextSearchAlgorithm, WANDQuery, WANDScorer,
};
use uqa_storage::{BlockMaxIndex, InvertedIndex, MemoryInvertedIndex};

#[path = "../../../uqa-storage/tests/cases/japanese/contract.rs"]
mod contract;

#[test]
fn japanese_occurrences_score_exact_reference_frequencies_in_all_wand_paths() {
    for case in contract::cases() {
        let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        contract::populate(&mut index, &case);
        contract::restore(&mut index, &case);
        contract::verify(&index, &case);
        let scorer = Arc::new(BM25Scorer::new(
            BM25Params::default(),
            Arc::new(index.field_stats("body").unwrap()),
        ));
        let mut terms = case.expected.terms.keys().cloned().collect::<Vec<_>>();
        terms.push(terms[0].clone());
        let mut bounds = BlockMaxIndex::new(1).unwrap();
        let mut expected = 0.0;
        for term in &terms {
            let frequency = case.expected.terms[term].len() as f64;
            // Three fields, two identical nonempty fields: df=2, dl/avgdl=3/2.
            let score =
                (1.0_f64 + 1.5 / 2.5).ln() * frequency / (frequency + 1.2 * (0.25 + 0.75 * 1.5));
            expected += score;
            bounds
                .set_block_maxes_key("docs", "body", term, vec![score; 2])
                .unwrap();
        }
        for algorithm in [TextSearchAlgorithm::Exhaustive, TextSearchAlgorithm::Wand] {
            let result = score_text_terms(
                &index,
                "docs",
                "body",
                &terms,
                &ScoringMode::BM25(BM25Params::default()),
                2,
                algorithm,
            )
            .unwrap();
            assert_eq!(result.algorithm, algorithm);
            assert_eq!(
                result
                    .entries
                    .iter()
                    .map(|row| row.doc_id)
                    .collect::<Vec<_>>(),
                [7, 65_536]
            );
            for row in result.entries {
                assert!((row.score - expected).abs() < 1e-12, "{}", case.id);
            }
        }
        for k in [1, 2] {
            let materialized = WANDQuery::new_keys(
                index.get_posting_lists_keys_bulk("body", &terms).unwrap(),
                vec![scorer.clone(); terms.len()],
                vec!["body".into(); terms.len()],
                terms.clone(),
                k,
            )
            .unwrap();
            let cursors = CursorWANDQuery::new_keys(
                index.posting_cursors_keys_bulk("body", &terms).unwrap(),
                vec![scorer.clone(); terms.len()],
                vec!["body".into(); terms.len()],
                terms.clone(),
                k,
            )
            .unwrap();
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
                assert_eq!(result.top_k.doc_ids().collect::<Vec<_>>(), [7, 65_536][..k]);
                for entry in &result.top_k {
                    assert!(
                        (entry.payload.score - expected).abs() < 1e-12,
                        "{}",
                        case.id
                    );
                }
            }
        }
    }
}
