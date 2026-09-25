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
    let mut ranked = Vec::new();
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
        ranked.push((entry.doc_id, score));
    }
    assert_eq!(ids, scores.posting_doc_ids);
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    assert_eq!(
        ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        scores.ranked_doc_ids
    );
    let top = index.search_knn(&scores.query, 1).unwrap();
    assert_eq!(top.iter().next().unwrap().doc_id, 2);
}
