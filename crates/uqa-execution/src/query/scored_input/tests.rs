//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::ScoredInput;
use uqa_core::ScoredEntry;

#[test]
fn score_cutoff_retains_the_complete_boundary_tie_group() {
    let mut input = ScoredInput::entries(
        vec![
            ScoredEntry {
                doc_id: 4,
                score: 0.25,
            },
            ScoredEntry {
                doc_id: 2,
                score: 0.75,
            },
            ScoredEntry {
                doc_id: 1,
                score: 1.0,
            },
            ScoredEntry {
                doc_id: 3,
                score: 0.75,
            },
        ],
        true,
    );

    input.retain_top_scores_with_ties(2);

    let ScoredInput::Entries { mut entries, .. } = input else {
        panic!("score-bearing input changed variants");
    };
    entries.sort_by_key(|entry| entry.doc_id);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect::<Vec<_>>(),
        vec![(1, 1.0), (2, 0.75), (3, 0.75)]
    );
}

#[test]
fn zero_score_cutoff_empties_score_bearing_entries_only() {
    let entries = vec![ScoredEntry {
        doc_id: 1,
        score: 1.0,
    }];
    let mut scored = ScoredInput::entries(entries.clone(), true);
    scored.retain_top_scores_with_ties(0);
    assert!(matches!(
        scored,
        ScoredInput::Entries { entries, .. } if entries.is_empty()
    ));

    let mut unscored = ScoredInput::entries(entries, false);
    unscored.retain_top_scores_with_ties(0);
    assert!(matches!(
        unscored,
        ScoredInput::Entries { entries, .. } if entries.len() == 1
    ));
}
