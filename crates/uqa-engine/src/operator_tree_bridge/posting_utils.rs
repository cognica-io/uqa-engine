//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Posting-list adapters and document-wise fusion utilities.

use super::{
    BTreeMap, BTreeSet, DocId, DriverResult, Payload, PostingEntry, PostingList, SQLError,
    ScoredEntry, Value,
};

/// Replay a posting list that the [`EngineDriver`] has already
/// computed. Used by fusion / boolean wrappers that take
/// `Arc<dyn Operator>` signals: the driver pre-executes each child
/// node and hands the result over as a [`StaticPostingList`].
pub(super) struct StaticPostingList {
    pub(super) pl: PostingList,
}

pub(super) fn static_operator(pl: PostingList) -> std::sync::Arc<dyn uqa_operators::Operator> {
    std::sync::Arc::new(StaticPostingList { pl })
}

pub(super) fn numeric_score(value: &Value) -> f64 {
    match value {
        Value::Int(value) => *value as f64,
        Value::Float(value) => *value,
        _ => 0.0,
    }
}

impl uqa_operators::base::Operator for StaticPostingList {
    fn execute(
        &self,
        _ctx: &uqa_operators::base::ExecutionContext,
    ) -> uqa_operators::base::OperatorResult {
        Ok(self.pl.clone())
    }
}

/// Combine a vector of per-signal posting lists into a single fused
/// posting list. `fuse` receives the per-signal probability vector
/// for one document and returns the fused score. Mirrors the
/// `collect_score_maps` + per-doc loop in
/// `uqa_operators::fusion_wrappers`.
pub(super) fn fuse_signals_with<F>(
    posting_lists: &[PostingList],
    fuse: F,
) -> DriverResult<PostingList>
where
    F: Fn(&[f64]) -> DriverResult<f64>,
{
    fuse_signal_batches_with(posting_lists, |probabilities| {
        probabilities.iter().map(|sample| fuse(sample)).collect()
    })
}

pub(super) fn fuse_signal_batches_with<F>(
    posting_lists: &[PostingList],
    fuse: F,
) -> DriverResult<PostingList>
where
    F: Fn(&[Vec<f64>]) -> DriverResult<Vec<f64>>,
{
    let (candidate_ids, probabilities) = fusion_probability_matrix(posting_lists);
    if candidate_ids.is_empty() {
        return Ok(PostingList::new());
    }
    let fused = fuse(&probabilities)?;
    if fused.len() != candidate_ids.len() {
        return Err(SQLError::Internal(format!(
            "fusion returned {} scores for {} candidates",
            fused.len(),
            candidate_ids.len()
        )));
    }
    let entries = candidate_ids
        .into_iter()
        .zip(fused)
        .map(|(doc_id, score)| {
            PostingEntry::new(
                doc_id,
                Payload {
                    score,
                    ..Default::default()
                },
            )
        })
        .collect();
    Ok(PostingList::from_sorted_unchecked(entries))
}

pub(super) fn fusion_probability_matrix(
    posting_lists: &[PostingList],
) -> (Vec<DocId>, Vec<Vec<f64>>) {
    let mut maps: Vec<BTreeMap<DocId, f64>> = Vec::with_capacity(posting_lists.len());
    let mut all_ids: BTreeSet<DocId> = BTreeSet::new();
    for pl in posting_lists {
        let mut m: BTreeMap<DocId, f64> = BTreeMap::new();
        for entry in pl {
            m.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        maps.push(m);
    }
    let total = all_ids.len();
    if total == 0 {
        return (Vec::new(), Vec::new());
    }
    let defaults: Vec<f64> = maps
        .iter()
        .map(|m| uqa_operators::hybrid::coverage_based_default(m.len(), total, 0.01))
        .collect();
    let mut candidate_ids = Vec::with_capacity(total);
    let mut probabilities = Vec::with_capacity(total);
    for doc_id in all_ids {
        let probs: Vec<f64> = maps
            .iter()
            .enumerate()
            .map(|(j, m)| *m.get(&doc_id).unwrap_or(&defaults[j]))
            .collect();
        candidate_ids.push(doc_id);
        probabilities.push(probs);
    }
    (candidate_ids, probabilities)
}

pub(super) fn scored_to_posting_list(scored: &[ScoredEntry]) -> PostingList {
    let mut entries: Vec<PostingEntry> = scored
        .iter()
        .map(|e| PostingEntry::new(e.doc_id, Payload::with_score(e.score)))
        .collect();
    entries.sort_by_key(|e| e.doc_id);
    PostingList::from_sorted_unchecked(entries)
}

pub(super) fn posting_list_to_scored(pl: &PostingList) -> Vec<ScoredEntry> {
    pl.entries()
        .iter()
        .map(|e| ScoredEntry {
            doc_id: e.doc_id,
            score: e.payload.score,
        })
        .collect()
}

pub(super) fn sparse_threshold_inline(
    source: &PostingList,
    threshold: f64,
) -> DriverResult<PostingList> {
    if !threshold.is_finite() {
        return Err(SQLError::TypeMismatch(format!(
            "sparse threshold must be finite, got {threshold}"
        )));
    }
    let entries = source
        .iter()
        .map(|entry| {
            if !entry.payload.score.is_finite() {
                return Err(SQLError::Internal(format!(
                    "sparse threshold source produced non-finite score {} for document {}",
                    entry.payload.score, entry.doc_id
                )));
            }
            let adjusted = entry.payload.score - threshold;
            if !adjusted.is_finite() {
                return Err(SQLError::Internal(format!(
                    "sparse threshold produced non-finite score for document {}",
                    entry.doc_id
                )));
            }
            if adjusted > 0.0 {
                Ok(Some(PostingEntry::new(
                    entry.doc_id,
                    Payload {
                        positions: entry.payload.positions.clone(),
                        score: adjusted,
                        fields: entry.payload.fields.clone(),
                    },
                )))
            } else {
                Ok(None)
            }
        })
        .collect::<DriverResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(PostingList::from_unsorted(entries))
}
