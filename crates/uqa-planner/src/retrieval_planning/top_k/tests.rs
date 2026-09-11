//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::retrieval_planning::testing::Inputs;
use uqa_operators::{TextTopKPlan, TextTopKStrategy};

#[test]
fn only_eligible_root_text_terms_read_live_planning_counts() {
    let inputs = Inputs::new(Err("must not probe"));
    let leaf = OperatorTree::Term {
        query: "rust search".into(),
        field: Some("body".into()),
        scoring: Some(TextScoringMode::BM25),
        top_k: None,
    };
    let nested = OperatorTree::Union(vec![leaf.clone()]);
    let OperatorTree::Union(children) = plan_bound_text_top_k(&inputs, "docs", nested, 4).unwrap()
    else {
        panic!("a nested retrieval must retain its unbounded child")
    };
    assert!(matches!(
        &children[..],
        [OperatorTree::Term { top_k: None, .. }]
    ));

    let mut planned = leaf.clone();
    let OperatorTree::Term { top_k, .. } = &mut planned else {
        unreachable!()
    };
    *top_k = Some(TextTopKPlan {
        k: 3,
        strategy: TextTopKStrategy::Wand,
    });
    assert!(
        matches!(plan_bound_text_top_k(&inputs, "docs", planned, 4).unwrap(),
        OperatorTree::Term { top_k: Some(plan), .. }
        if plan.k == 3 && plan.strategy == TextTopKStrategy::Wand)
    );
    assert!(inputs.text_reads.borrow().is_empty());

    assert!(
        matches!(plan_bound_text_top_k(&inputs, "docs", leaf, 4).unwrap(),
        OperatorTree::Term { query, field: Some(field), top_k: Some(plan), .. }
        if query == "rust search" && field == "body" && plan.k == 4
            && plan.strategy == TextTopKStrategy::BlockMaxWand)
    );
    assert_eq!(
        *inputs.text_reads.borrow(),
        [("docs".into(), "body".into(), "rust search".into())]
    );
}
