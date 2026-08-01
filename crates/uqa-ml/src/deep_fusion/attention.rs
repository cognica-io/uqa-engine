//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attention and final posting-list construction.

use super::{
    runtime_filled_vec, runtime_model_error, runtime_vec_with_capacity, sigmoid,
    usize_to_f64_exact, BTreeMap, ForwardState, OperatorResult, Payload, PostingEntry, PostingList,
    StorageBackendResult, Value,
};

pub(super) fn apply_attention(state: &mut ForwardState) -> StorageBackendResult<()> {
    let ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if ids.len() < 2 {
        return Ok(());
    }
    let dim = state.channel_map[&ids[0]].len();
    if dim == 0 {
        return Ok(());
    }
    let scale = usize_to_f64_exact(dim, "attention channel count")?.sqrt();
    let mut xs = runtime_vec_with_capacity(ids.len(), "attention input rows")?;
    for id in &ids {
        xs.push(
            crate::backend::try_clone_slice(&state.channel_map[id], "attention input channels")
                .map_err(|error| runtime_model_error(error.to_string()))?,
        );
    }
    if let Some((row, values)) = xs
        .iter()
        .enumerate()
        .find(|(_, values)| values.len() != dim)
    {
        return Err(runtime_model_error(format!(
            "attention row {row} has {} channels, expected {dim}",
            values.len()
        )));
    }
    // Scaled dot-product attention with Q=K=V=X.
    let mut out_rows = runtime_vec_with_capacity(ids.len(), "attention output rows")?;
    for q in &xs {
        // Compute attention logits over every key.
        let mut logits = runtime_vec_with_capacity(xs.len(), "attention logits")?;
        for k in &xs {
            let dot: f64 = q.iter().zip(k.iter()).map(|(a, b)| a * b).sum();
            logits.push(dot / scale);
        }
        let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut exps = runtime_vec_with_capacity(logits.len(), "attention exponentials")?;
        exps.extend(logits.iter().map(|x| (x - max).exp()));
        let sum: f64 = exps.iter().sum();
        let weights: Vec<f64> = if sum > 0.0 {
            let mut weights = runtime_vec_with_capacity(exps.len(), "attention weights")?;
            weights.extend(exps.iter().map(|x| x / sum));
            weights
        } else {
            runtime_filled_vec(
                xs.len(),
                1.0 / usize_to_f64_exact(xs.len(), "attention row count")?,
                "attention fallback weights",
            )?
        };
        let mut combined = runtime_filled_vec(dim, 0.0f64, "attention combined channels")?;
        for (w, v) in weights.iter().zip(xs.iter()) {
            for i in 0..dim {
                combined[i] += w * v[i];
            }
        }
        out_rows.push(combined);
    }
    for (did, row) in ids.into_iter().zip(out_rows) {
        state.channel_map.insert(did, row);
    }
    Ok(())
}

pub(super) fn build_result(
    channel_map: &BTreeMap<u64, Vec<f64>>,
    num_channels: usize,
    softmax_applied: bool,
) -> OperatorResult {
    if channel_map.is_empty() {
        return Ok(PostingList::new());
    }
    let mut entries = runtime_vec_with_capacity(channel_map.len(), "deep-fusion result entries")?;
    for (doc_id, vec) in channel_map {
        if vec.len() != num_channels || vec.is_empty() {
            return Err(runtime_model_error(format!(
                "deep-fusion result for doc {doc_id} has {} channels, expected {num_channels}",
                vec.len()
            )));
        }
        if softmax_applied {
            let score = vec.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut payload = Payload::with_score(score);
            let mut class_probs = runtime_vec_with_capacity(vec.len(), "class probabilities")?;
            class_probs.extend(vec.iter().map(|value| Value::Float(*value)));
            payload
                .fields
                .insert("class_probs".into(), Value::List(class_probs));
            entries.push(PostingEntry::new(*doc_id, payload));
        } else if num_channels == 1 {
            let score = sigmoid(vec[0]);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(score)));
        } else {
            let max_sigmoid = vec
                .iter()
                .map(|x| sigmoid(*x))
                .fold(f64::NEG_INFINITY, f64::max);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(max_sigmoid)));
        }
    }
    entries.sort_by_key(|e| e.doc_id);
    Ok(PostingList::from_sorted_unchecked(entries))
}
