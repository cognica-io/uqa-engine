//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-field search operator cost-estimate coverage.

use uqa_core::IndexStats;
use uqa_operators::{MultiFieldSearchOperator, Operator};

#[test]
fn multi_field_search_cost_scales_with_field_count() {
    let op = MultiFieldSearchOperator::new(vec!["title".into(), "body".into()], "test", None);
    let stats = IndexStats::new(100);
    assert_eq!(op.cost_estimate(&stats), 200.0);
}

#[test]
fn raw_nori_terms_match_and_score_through_the_typed_multi_field_operator() {
    use std::{collections::BTreeMap, sync::Arc};
    use uqa_storage::InvertedIndex;
    let config: uqa_analysis::Analyzer = match serde_json::from_str(
        r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"🙂a 가 나"},"token_filters":[]}"#,
    ) {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return;
        }
    };
    let mut index = uqa_storage::MemoryInvertedIndex::new(config);
    index
        .try_add_documents(
            [(1, "🙂a"), (2, "서울")]
                .into_iter()
                .map(|(id, text)| (id, BTreeMap::from([("body".into(), text.into())])))
                .collect(),
        )
        .unwrap();
    let context = uqa_operators::ExecutionContext::new().with_inverted_index(Arc::new(index));
    let result = MultiFieldSearchOperator::new(vec!["body".into()], "🙂a", None)
        .execute(&context)
        .unwrap();
    assert_eq!(result.doc_ids().collect::<Vec<_>>(), [1]);
    assert!(result.entries()[0].payload.score.is_finite());
    assert!(result.entries()[0].payload.score > 0.0);
}
