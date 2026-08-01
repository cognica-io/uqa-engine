//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedding, signal, dense, pooling, normalization, and dropout kernels.

use super::{
    apply_gating, log_odds_conjunction_weighted, runtime_filled_vec, runtime_model_error,
    runtime_vec_with_capacity, safe_logit, usize_to_f64_exact, Arc, BTreeMap, DeepFusionOperator,
    ExecutionContext, ForwardState, Gating, GlobalPoolMethod, Operator, StorageBackendError,
    StorageBackendResult,
};

pub(super) fn apply_embed(embedding: &[f64], state: &mut ForwardState) -> StorageBackendResult<()> {
    for (i, val) in embedding.iter().enumerate() {
        let index = u64::try_from(i)
            .map_err(|_| runtime_model_error("embedding index exceeds the u64 range"))?;
        let doc_id = index
            .checked_add(1)
            .ok_or_else(|| runtime_model_error("embedding index exceeds the document-ID range"))?;
        state.channel_map.insert(doc_id, vec![*val]);
    }
    state.num_channels = 1;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn apply_signal(
    signals: &[Arc<dyn Operator>],
    ctx: &ExecutionContext,
    alpha: f64,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let mut posting_lists = runtime_vec_with_capacity(signals.len(), "deep-fusion signals")?;
    for signal in signals {
        posting_lists.push(signal.execute(ctx)?);
    }
    let mut score_maps = runtime_vec_with_capacity(signals.len(), "deep-fusion score maps")?;
    let mut all_doc_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for pl in &posting_lists {
        let mut smap: BTreeMap<u64, f64> = BTreeMap::new();
        for entry in pl.entries() {
            if !entry.payload.score.is_finite() || !(0.0..=1.0).contains(&entry.payload.score) {
                return Err(runtime_model_error(format!(
                    "deep-fusion signal score for doc {} must be a finite probability in [0, 1], got {}",
                    entry.doc_id, entry.payload.score
                )));
            }
            smap.insert(entry.doc_id, entry.payload.score);
            all_doc_ids.insert(entry.doc_id);
        }
        score_maps.push(smap);
    }
    if all_doc_ids.is_empty() {
        return Ok(());
    }
    let total = all_doc_ids.len();
    let mut defaults = runtime_vec_with_capacity(score_maps.len(), "signal defaults")?;
    defaults.extend(
        score_maps
            .iter()
            .map(|map| DeepFusionOperator::coverage_default(map.len(), total)),
    );
    for doc_id in &all_doc_ids {
        let mut probs = runtime_vec_with_capacity(score_maps.len(), "signal probabilities")?;
        probs.extend(
            score_maps
                .iter()
                .enumerate()
                .map(|(i, map)| map.get(doc_id).copied().unwrap_or(defaults[i])),
        );
        let fused = if probs.len() == 1 {
            probs[0]
        } else {
            let n = probs.len();
            let weights = runtime_filled_vec(
                n,
                1.0 / usize_to_f64_exact(n, "signal count")?,
                "deep-fusion signal weights",
            )?;
            log_odds_conjunction_weighted(&probs, &weights, alpha)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
        };
        let layer_logit = apply_gating(safe_logit(fused), gating);
        let n = state.num_channels;
        if !state.channel_map.contains_key(doc_id) {
            let channels = runtime_filled_vec(n, 0.0, "deep-fusion signal channels")?;
            state.channel_map.insert(*doc_id, channels);
        }
        let entry = state.channel_map.get_mut(doc_id).ok_or_else(|| {
            runtime_model_error(format!("missing signal output for doc {doc_id}"))
        })?;
        entry[0] += layer_logit;
    }
    Ok(())
}

pub(super) fn apply_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let doc_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    for did in doc_ids {
        let input = state
            .channel_map
            .get(&did)
            .ok_or_else(|| runtime_model_error(format!("missing dense input for doc {did}")))?;
        if input.len() != input_channels {
            return Err(runtime_model_error(format!(
                "dense input for doc {did} has {} channels, expected {input_channels}",
                input.len()
            )));
        }
        let mut out = runtime_filled_vec(output_channels, 0.0f64, "dense output channels")?;
        for o in 0..output_channels {
            let mut acc = bias[o];
            for i in 0..input_channels {
                acc += weights[o * input_channels + i] * input[i];
            }
            out[o] = apply_gating(acc, gating);
        }
        state.channel_map.insert(did, out);
    }
    state.num_channels = output_channels;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn apply_flatten(state: &mut ForwardState) -> StorageBackendResult<()> {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return Ok(());
    }
    let flat_len = sorted_ids.iter().try_fold(0usize, |length, doc_id| {
        length
            .checked_add(state.channel_map[doc_id].len())
            .ok_or_else(|| runtime_model_error("flattened channel count overflows usize"))
    })?;
    let mut flat = runtime_vec_with_capacity(flat_len, "flattened channels")?;
    for did in &sorted_ids {
        if let Some(v) = state.channel_map.get(did) {
            flat.extend_from_slice(v);
        }
    }
    let new_n = flat.len();
    let rep = sorted_ids[0];
    state.channel_map.clear();
    state.channel_map.insert(rep, flat);
    state.num_channels = new_n;
    Ok(())
}

pub(super) fn apply_global_pool(
    method: GlobalPoolMethod,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return Ok(());
    }
    let n_dims = state.channel_map[&sorted_ids[0]].len();
    let mut sums = runtime_filled_vec(n_dims, 0.0f64, "global-pool sums")?;
    let mut maxes = runtime_filled_vec(n_dims, f64::NEG_INFINITY, "global-pool maxima")?;
    for did in &sorted_ids {
        let v = &state.channel_map[did];
        if v.len() != n_dims {
            return Err(runtime_model_error(format!(
                "global-pool input for doc {did} has {} channels, expected {n_dims}",
                v.len()
            )));
        }
        for i in 0..n_dims {
            let x = v[i];
            sums[i] += x;
            if x > maxes[i] {
                maxes[i] = x;
            }
        }
    }
    let count = usize_to_f64_exact(sorted_ids.len(), "global-pool row count")?;
    let pooled: Vec<f64> = match method {
        GlobalPoolMethod::Avg => {
            let mut averages = runtime_vec_with_capacity(n_dims, "global-pool averages")?;
            averages.extend(sums.iter().map(|s| s / count));
            averages
        }
        GlobalPoolMethod::Max => crate::backend::try_clone_slice(&maxes, "global-pool output")
            .map_err(|error| runtime_model_error(error.to_string()))?,
        GlobalPoolMethod::AvgMax => {
            let capacity = n_dims
                .checked_mul(2)
                .ok_or_else(|| runtime_model_error("AvgMax output width overflows usize"))?;
            let mut combined = runtime_vec_with_capacity(capacity, "AvgMax output channels")?;
            combined.extend(sums.iter().map(|s| s / count));
            combined.extend(maxes.iter().copied());
            combined
        }
    };
    let new_n = pooled.len();
    let rep = sorted_ids[0];
    state.channel_map.clear();
    state.channel_map.insert(rep, pooled);
    state.num_channels = new_n;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn apply_softmax(state: &mut ForwardState) -> StorageBackendResult<()> {
    for vec_ref in state.channel_map.values_mut() {
        if vec_ref.is_empty() {
            return Err(runtime_model_error("softmax input has zero channels"));
        }
        let max = vec_ref.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut exps = runtime_vec_with_capacity(vec_ref.len(), "softmax exponentials")?;
        exps.extend(vec_ref.iter().map(|x| (x - max).exp()));
        let sum: f64 = exps.iter().sum();
        if sum > 0.0 {
            for x in &mut exps {
                *x /= sum;
            }
        } else {
            let n = usize_to_f64_exact(exps.len(), "softmax channel count")?;
            for x in &mut exps {
                *x = 1.0 / n;
            }
        }
        *vec_ref = exps;
    }
    state.softmax_applied = true;
    Ok(())
}

pub(super) fn apply_batch_norm(epsilon: f64, state: &mut ForwardState) -> StorageBackendResult<()> {
    if state.channel_map.len() < 2 {
        return Ok(());
    }
    let dim = state.channel_map.values().next().map_or(0, Vec::len);
    if dim == 0 {
        return Ok(());
    }
    let mut means = runtime_filled_vec(dim, 0.0f64, "batch-normalization means")?;
    for v in state.channel_map.values() {
        for i in 0..dim {
            means[i] += v[i];
        }
    }
    let n = usize_to_f64_exact(state.channel_map.len(), "batch-normalization row count")?;
    for m in &mut means {
        *m /= n;
    }
    let mut vars = runtime_filled_vec(dim, 0.0f64, "batch-normalization variances")?;
    for v in state.channel_map.values() {
        for i in 0..dim {
            let d = v[i] - means[i];
            vars[i] += d * d;
        }
    }
    for v in &mut vars {
        *v /= n;
    }
    for v in state.channel_map.values_mut() {
        for i in 0..dim {
            v[i] = (v[i] - means[i]) / (vars[i] + epsilon).sqrt();
        }
    }
    Ok(())
}

pub(super) fn apply_dropout(p: f64, state: &mut ForwardState) {
    let scale = 1.0 - p;
    for v in state.channel_map.values_mut() {
        for x in v.iter_mut() {
            *x *= scale;
        }
    }
}
