//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Centroid probing and exact candidate scoring.

use std::collections::BTreeMap;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::vector_index::{cosine_similarity, select_top_k_scored};

pub(super) fn nearest_centroids(
    vector: &[f32],
    centroids: &[Vec<f32>],
    nprobe: usize,
) -> Vec<usize> {
    let mut query = vector.to_vec();
    normalize(&mut query);
    let mut scored = centroids
        .iter()
        .enumerate()
        .map(|(index, centroid)| (index, dot(&query, centroid)))
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .take(nprobe.max(1))
        .map(|(index, _)| index)
        .collect()
}

pub(super) fn scored_posting_list(
    query: &[f32],
    entries: &[(DocId, Vec<f32>)],
    k: usize,
) -> PostingList {
    let mut best_by_doc = BTreeMap::<DocId, f32>::new();
    for (doc_id, vector) in entries {
        let similarity = cosine_similarity(query, vector);
        best_by_doc
            .entry(*doc_id)
            .and_modify(|best| *best = best.max(similarity))
            .or_insert(similarity);
    }
    let mut scored = best_by_doc.into_iter().collect::<Vec<_>>();
    select_top_k_scored(&mut scored, k);
    scored.sort_by_key(|(doc_id, _)| *doc_id);
    PostingList::from_sorted_unchecked(
        scored
            .into_iter()
            .map(|(doc_id, similarity)| {
                PostingEntry::new(doc_id, Payload::with_score(f64::from(similarity)))
            })
            .collect(),
    )
}

fn normalize(vector: &mut [f32]) {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 1e-12 {
        for value in vector {
            *value /= magnitude;
        }
    }
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(x, y)| x * y).sum()
}
