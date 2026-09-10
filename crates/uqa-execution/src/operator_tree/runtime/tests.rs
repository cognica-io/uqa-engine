//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::validate_text_top_k_placement;
use uqa_operators::{OperatorTree, TextScoringMode};

fn physical_text_leaf() -> OperatorTree {
    OperatorTree::Term {
        query: "rust search".into(),
        field: Some("body".into()),
        scoring: Some(TextScoringMode::BM25),
        top_k: Some(uqa_operators::TextTopKPlan {
            k: 10,
            strategy: uqa_operators::TextTopKStrategy::Wand,
        }),
    }
}

#[test]
fn physical_text_limit_is_rejected_below_a_parent() {
    assert!(validate_text_top_k_placement(&physical_text_leaf()).is_ok());
    assert!(
        validate_text_top_k_placement(&OperatorTree::Union(vec![physical_text_leaf()])).is_err()
    );
}
