//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{PostingList, Predicate, Value};
use uqa_operators::{
    BayesianEvidenceFusionOperator, CosineProbabilityOperator, ExecutionContext, FilterOperator,
    HybridTextVectorOperator, KNNOperator, Operator, SemanticFilterOperator, TermOperator,
    VectorSimilarityOperator,
};
use uqa_storage::{
    read_control::StorageReadControl, DocumentStore, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, VectorIndex,
};

#[path = "diskann/source.rs"]
mod source;

fn context(index: Arc<dyn VectorIndex>) -> ExecutionContext {
    let mut documents = MemoryDocumentStore::new();
    let mut text = MemoryInvertedIndex::new(uqa_analysis::Analyzer::default());
    for (document, keep, term) in [
        (1, false, "needle"),
        (2, true, "other"),
        (3, true, "needle"),
        (4, true, "needle"),
    ] {
        documents
            .put(
                document,
                BTreeMap::from([("keep".into(), Value::Bool(keep))]),
            )
            .unwrap();
        text.try_add_document(document, BTreeMap::from([("text".into(), term.into())]))
            .unwrap();
    }
    ExecutionContext::new()
        .with_vector_index("vector", index)
        .with_document_store(Arc::new(documents))
        .with_inverted_index(Arc::new(text))
}

fn scores(postings: &PostingList) -> Vec<(u64, f64)> {
    postings
        .iter()
        .map(|posting| (posting.doc_id, posting.payload.score))
        .collect()
}

#[test]
fn diskann_knn_and_ordinary_filters_preserve_raw_tensor_scores_and_do_not_refill() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (index, exact) = source::indexes(&control);
    let context = context(index);
    let exact_context = self::context(exact);
    let all = KNNOperator::new(vec![1.0, 0.0], 10, "vector");
    for _ in 0..2 {
        let actual = all.execute(&context).unwrap();
        assert_eq!(scores(&actual), [(1, 1.0), (2, 0.0), (4, -1.0)]);
        assert_eq!(actual, all.execute(&exact_context).unwrap());
    }
    let filter = FilterOperator::new(
        "keep",
        Predicate::Equals(Value::Bool(true)),
        Some(Arc::new(KNNOperator::new(vec![1.0, 0.0], 1, "vector"))),
    );
    assert!(
        filter.execute(&context).unwrap().is_empty(),
        "ordinary filter runs after KNN, without retrieving a replacement"
    );
    assert_eq!(
        filter.execute(&context).unwrap(),
        filter.execute(&exact_context).unwrap()
    );
    let threshold = VectorSimilarityOperator::new(vec![1.0, 0.0], 0.0, "vector");
    assert_eq!(
        scores(&threshold.execute(&context).unwrap()),
        [(1, 1.0), (2, 0.0)]
    );
}

#[test]
fn diskann_hybrid_and_probability_consumers_keep_the_existing_composition() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (index, exact) = source::indexes(&control);
    let context = context(index);
    let exact_context = self::context(exact);
    let hybrid = HybridTextVectorOperator::new("needle", "text", vec![1.0, 0.0], -1.0, "vector");
    let actual = hybrid.execute(&context).unwrap();
    assert_eq!(actual.doc_ids().collect::<Vec<_>>(), [1, 4]);
    assert_eq!(actual, hybrid.execute(&exact_context).unwrap());
    let semantic = SemanticFilterOperator::new(
        Arc::new(TermOperator::new("needle", "text")),
        VectorSimilarityOperator::new(vec![1.0, 0.0], 0.0, "vector"),
    );
    assert_eq!(
        semantic
            .execute(&context)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        semantic.execute(&context).unwrap(),
        semantic.execute(&exact_context).unwrap()
    );
    let probability = Arc::new(CosineProbabilityOperator::new(Arc::new(KNNOperator::new(
        vec![1.0, 0.0],
        10,
        "vector",
    ))));
    assert_eq!(
        scores(&probability.execute(&context).unwrap()),
        [(1, 1.0 - 1e-10), (2, 0.5), (4, 1e-10)]
    );
    let fusion = BayesianEvidenceFusionOperator::new(vec![probability], 0.25);
    let actual = fusion.execute(&context).unwrap();
    assert_eq!(actual, fusion.execute(&exact_context).unwrap());
    assert!((actual.entries()[1].payload.score - 0.25).abs() < 1e-12);
    control.cancellation().cancel();
    assert!(hybrid.execute(&context).is_err());
    assert!(semantic.execute(&context).is_err());
    assert!(fusion.execute(&context).is_err());
}
