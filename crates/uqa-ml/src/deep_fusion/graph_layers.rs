//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph propagation, convolution, and neighborhood pooling kernels.

use super::{
    apply_gating, runtime_filled_vec, runtime_model_error, runtime_vec_with_capacity, safe_logit,
    sigmoid, usize_to_f64_exact, AggregationKind, BTreeMap, Direction, ExecutionContext,
    ForwardState, Gating, PoolMethod, StorageBackendResult, PROB_EPSILON,
};

pub(super) fn neighbors_of(
    ctx: &ExecutionContext,
    vid: u64,
    label: &str,
    direction: Direction,
) -> StorageBackendResult<Vec<u64>> {
    let Some(graph) = ctx.graph.as_ref() else {
        return Err(runtime_model_error(
            "graph-neighbor lookup requires an execution graph",
        ));
    };
    graph.neighbors(vid, label, direction)
}

pub(super) fn apply_propagate(
    edge_label: &str,
    aggregation: AggregationKind,
    direction: Direction,
    ctx: &ExecutionContext,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph propagation requires an execution graph",
        ));
    }
    // Convert channel 0 to a probability map.
    let mut prob_map: BTreeMap<u64, f64> = BTreeMap::new();
    for (did, vec) in &state.channel_map {
        prob_map.insert(*did, sigmoid(vec[0]));
    }
    // Discover neighbors of every existing doc to expand the working set.
    let mut all_vertices: std::collections::BTreeSet<u64> =
        state.channel_map.keys().copied().collect();
    for vid in state.channel_map.keys().copied().collect::<Vec<_>>() {
        for nb in neighbors_of(ctx, vid, edge_label, direction)? {
            all_vertices.insert(nb);
        }
    }
    let mut new_map: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for vid in &all_vertices {
        let mut neighbor_probs: Vec<f64> = Vec::new();
        for nb in neighbors_of(ctx, *vid, edge_label, direction)? {
            if let Some(p) = prob_map.get(&nb) {
                neighbor_probs.push(*p);
            }
        }
        if neighbor_probs.is_empty() {
            if let Some(existing) = state.channel_map.get(vid) {
                let existing = crate::backend::try_clone_slice(
                    existing,
                    "graph-propagation unchanged channels",
                )
                .map_err(|error| runtime_model_error(error.to_string()))?;
                new_map.insert(*vid, existing);
            }
            continue;
        }
        let agg = match aggregation {
            AggregationKind::Mean => {
                neighbor_probs.iter().sum::<f64>()
                    / usize_to_f64_exact(neighbor_probs.len(), "graph-propagation neighbor count")?
            }
            AggregationKind::Sum => neighbor_probs.iter().sum::<f64>().min(1.0 - PROB_EPSILON),
            AggregationKind::Max => neighbor_probs
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        };
        let propagated_logit = apply_gating(safe_logit(agg), gating);
        let mut new_vec = match state.channel_map.get(vid) {
            Some(existing) => {
                crate::backend::try_clone_slice(existing, "graph-propagation output channels")
                    .map_err(|error| runtime_model_error(error.to_string()))?
            }
            None => {
                runtime_filled_vec(state.num_channels, 0.0, "graph-propagation output channels")?
            }
        };
        if new_vec.is_empty() {
            new_vec =
                runtime_filled_vec(state.num_channels, 0.0, "graph-propagation output channels")?;
        }
        new_vec[0] += propagated_logit;
        new_map.insert(*vid, new_vec);
    }
    state.channel_map = new_map;
    Ok(())
}

pub(super) fn apply_conv(
    edge_label: &str,
    hop_weights: &[f64],
    direction: Direction,
    ctx: &ExecutionContext,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph convolution requires an execution graph",
        ));
    }
    if hop_weights.is_empty() {
        return Err(runtime_model_error(
            "graph convolution requires at least one hop weight",
        ));
    }
    let total_w: f64 = hop_weights.iter().sum();
    if !total_w.is_finite() || total_w <= 0.0 {
        return Err(runtime_model_error(
            "graph convolution hop weights must have a finite positive sum",
        ));
    }
    let mut norm = runtime_vec_with_capacity(hop_weights.len(), "graph-convolution weights")?;
    norm.extend(hop_weights.iter().map(|w| w / total_w));
    let mut val_map: BTreeMap<u64, f64> = BTreeMap::new();
    for (did, vec) in &state.channel_map {
        val_map.insert(*did, sigmoid(vec[0]));
    }
    let kernel_hops = hop_weights.len() - 1;
    let mut new_map: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for vid in state.channel_map.keys().copied().collect::<Vec<_>>() {
        let mut weighted = 0.0f64;
        if let Some(p) = val_map.get(&vid) {
            weighted += norm[0] * p;
        }
        let mut current_frontier: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::from([vid]);
        let mut visited: std::collections::BTreeSet<u64> = current_frontier.clone();
        for hop_weight in norm.iter().copied().skip(1).take(kernel_hops) {
            let mut next_frontier: std::collections::BTreeSet<u64> =
                std::collections::BTreeSet::new();
            for fv in &current_frontier {
                for nb in neighbors_of(ctx, *fv, edge_label, direction)? {
                    if visited.insert(nb) {
                        next_frontier.insert(nb);
                    }
                }
            }
            if !next_frontier.is_empty() {
                let hop_vals: Vec<f64> = next_frontier
                    .iter()
                    .filter_map(|nb| val_map.get(nb).copied())
                    .collect();
                if !hop_vals.is_empty() {
                    let mean = hop_vals.iter().sum::<f64>()
                        / usize_to_f64_exact(hop_vals.len(), "graph-convolution hop count")?;
                    weighted += hop_weight * mean;
                }
            }
            current_frontier = next_frontier;
        }
        let conv_logit = apply_gating(
            safe_logit(weighted.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON)),
            gating,
        );
        let mut new_vec = crate::backend::try_clone_slice(
            &state.channel_map[&vid],
            "graph-convolution output channels",
        )
        .map_err(|error| runtime_model_error(error.to_string()))?;
        new_vec[0] += conv_logit;
        new_map.insert(vid, new_vec);
    }
    state.channel_map = new_map;
    Ok(())
}

pub(super) fn apply_pool(
    edge_label: &str,
    pool_size: usize,
    method: PoolMethod,
    direction: Direction,
    ctx: &ExecutionContext,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph pooling requires an execution graph",
        ));
    }
    if pool_size == 1 {
        return Ok(());
    }
    if pool_size == 0 {
        return Err(runtime_model_error(
            "graph pooling size must be greater than zero",
        ));
    }
    let mut remaining: std::collections::BTreeSet<u64> =
        state.channel_map.keys().copied().collect();
    let mut pooled: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    while let Some(seed) = remaining.iter().copied().next() {
        remaining.remove(&seed);
        let mut group: Vec<u64> = vec![seed];
        let mut frontier: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::from([seed]);
        let mut visited: std::collections::BTreeSet<u64> = frontier.clone();
        while group.len() < pool_size && !frontier.is_empty() {
            let mut next: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            for fv in &frontier {
                for nb in neighbors_of(ctx, *fv, edge_label, direction)? {
                    if visited.insert(nb) {
                        next.insert(nb);
                        if remaining.remove(&nb) {
                            group.push(nb);
                            if group.len() >= pool_size {
                                break;
                            }
                        }
                    }
                }
                if group.len() >= pool_size {
                    break;
                }
            }
            frontier = next;
        }
        let dim = state.channel_map.get(&seed).map_or(0, Vec::len);
        let mut agg = match method {
            PoolMethod::Avg => runtime_filled_vec(dim, 0.0f64, "graph-pool average")?,
            PoolMethod::Max => runtime_filled_vec(dim, f64::NEG_INFINITY, "graph-pool maximum")?,
        };
        for g in &group {
            let v = &state.channel_map[g];
            if v.len() != dim {
                return Err(runtime_model_error(format!(
                    "graph-pool input for doc {g} has {} channels, expected {dim}",
                    v.len()
                )));
            }
            for (i, slot) in agg.iter_mut().enumerate() {
                let x = v[i];
                match method {
                    PoolMethod::Avg => *slot += x,
                    PoolMethod::Max => {
                        if x > *slot {
                            *slot = x;
                        }
                    }
                }
            }
        }
        if matches!(method, PoolMethod::Avg) {
            let n = usize_to_f64_exact(group.len(), "graph-pool group size")?;
            for x in &mut agg {
                *x /= n;
            }
        }
        pooled.insert(seed, agg);
    }
    state.channel_map = pooled;
    Ok(())
}
