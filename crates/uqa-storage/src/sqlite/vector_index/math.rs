//! Vector normalization, centroid probing, and result scoring.

use super::{cosine_similarity, select_top_k_scored, DocId, Payload, PostingEntry, PostingList};

pub(super) fn l2_normalise(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 1e-12 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

pub(super) fn nearest_centroids_for_raw(
    vector: &[f32],
    centroids: &[Vec<f32>],
    nprobe: usize,
) -> Vec<usize> {
    let mut q = vector.to_vec();
    l2_normalise(&mut q);
    let mut scored: Vec<(usize, f32)> = centroids
        .iter()
        .enumerate()
        .map(|(i, centroid)| (i, dot(&q, centroid)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored
        .into_iter()
        .take(nprobe.max(1))
        .map(|(idx, _)| idx)
        .collect()
}

pub(super) fn scored_posting_list(
    query: &[f32],
    entries: &[(DocId, Vec<f32>)],
    k: usize,
) -> PostingList {
    let mut best_by_doc: std::collections::BTreeMap<DocId, f32> = std::collections::BTreeMap::new();
    for (doc_id, vector) in entries {
        let sim = cosine_similarity(query, vector);
        best_by_doc
            .entry(*doc_id)
            .and_modify(|best| {
                if sim > *best {
                    *best = sim;
                }
            })
            .or_insert(sim);
    }
    let mut scored: Vec<(DocId, f32)> = best_by_doc.into_iter().collect();
    select_top_k_scored(&mut scored, k);
    scored.sort_by_key(|(id, _)| *id);
    let entries = scored
        .into_iter()
        .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
        .collect::<Vec<_>>();
    PostingList::from_sorted_unchecked(entries)
}
