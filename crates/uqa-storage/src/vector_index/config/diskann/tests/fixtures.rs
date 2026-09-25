//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::Deserialize;
use uqa_core::DocId;

use crate::{MemoryVectorIndex, VectorIndex};

#[derive(Deserialize)]
struct Fixture {
    scores: Scores,
}

#[derive(Deserialize)]
struct Scores {
    query: Vec<f32>,
    documents: Vec<Document>,
    ranked_doc_ids: Vec<DocId>,
    posting_doc_ids: Vec<DocId>,
}

#[derive(Deserialize)]
struct Document {
    id: DocId,
    vectors: Vec<Vec<f32>>,
    score_bits: Option<String>,
}

#[test]
fn independent_tensor_score_and_tie_fixtures_match_canonical_search() {
    let fixture: Fixture = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/diskann/reference.json"
    )))
    .unwrap();
    let scores = fixture.scores;
    let mut index = MemoryVectorIndex::new(2);
    for document in &scores.documents {
        index
            .add_many(document.id, document.vectors.clone())
            .unwrap();
    }
    let found = index
        .search_knn(&scores.query, scores.documents.len())
        .unwrap();
    let mut ids = Vec::new();
    for entry in &found {
        let score = entry.payload.score;
        let expected = scores
            .documents
            .iter()
            .find(|doc| doc.id == entry.doc_id)
            .unwrap();
        let bits = u32::from_str_radix(expected.score_bits.as_ref().unwrap(), 16).unwrap();
        assert_eq!(
            score,
            f64::from(f32::from_bits(bits)),
            "document {}",
            entry.doc_id
        );
        ids.push(entry.doc_id);
    }
    assert_eq!(ids, scores.posting_doc_ids);
    for k in 0..=scores.documents.len() {
        let mut expected = scores.ranked_doc_ids[..k.min(scores.ranked_doc_ids.len())].to_vec();
        expected.sort_unstable();
        let selected = index.search_knn(&scores.query, k).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|entry| entry.doc_id)
                .collect::<Vec<_>>(),
            expected,
            "k = {k}"
        );
        for entry in &selected {
            let full = found
                .iter()
                .find(|full| full.doc_id == entry.doc_id)
                .unwrap();
            assert_eq!(
                entry.payload.score, full.payload.score,
                "k = {k}, document {}",
                entry.doc_id
            );
        }
    }
}
