//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph traversal, pattern, RPQ, temporal, and sampling estimates.

use super::{
    rpq_label_count, Arc, BTreeMap, CardinalityEstimator, GraphPatternIR, GraphStats,
    GraphStoreSampler, IndexStats, OperatorTree, TemporalFilterIR, VertexConstraint, XorShiftRng,
};

impl CardinalityEstimator {
    /// Estimate traversal cardinality from graph statistics.
    pub(super) fn estimate_traverse(
        &self,
        label: Option<&str>,
        hops: usize,
        n: f64,
        temporal_filter: Option<&TemporalFilterIR>,
    ) -> f64 {
        let branching = if let Some(gs) = self.graph_stats.as_ref() {
            if let (Some(name), false) = (label, gs.label_degree_map.is_empty()) {
                gs.label_degree_map
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| gs.avg_out_degree * gs.label_selectivity(Some(name)))
            } else {
                gs.avg_out_degree * gs.label_selectivity(label)
            }
        } else {
            (n * 0.1).min(10.0)
        };

        // Compute `branching ** max_hops` directly: hops=0 collapses to a
        // single empty path (1), so `result = min(n, branching ** hops)`
        // to the reference.
        let hops_f = hops as f64;
        let mut result = n.min(branching.powf(hops_f));

        if let (Some(tf), Some(gs)) = (temporal_filter, self.graph_stats.as_ref()) {
            result *= self.temporal_selectivity(tf, gs);
        }
        result
    }

    /// Estimate pattern-match cardinality from graph statistics or sampling.
    pub(super) fn estimate_pattern_match(&self, pattern: &GraphPatternIR, n: f64) -> f64 {
        let k = pattern.vertex_patterns.len();
        let e = pattern.edge_patterns.len();

        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };

            // Fixed-size random-walk heuristic for large graphs.
            if nv > 10_000.0 && self.graph_store.is_some() {
                if let Some(sampled) = self.sample_graph_cardinality(pattern, 100) {
                    return sampled.max(1.0);
                }
            }

            let density = gs.edge_density();

            let mut label_sel = 1.0;
            for ep in &pattern.edge_patterns {
                label_sel *= gs.label_selectivity(ep.label.as_deref());
            }

            let mut vertex_sel = 1.0;
            if !gs.vertex_label_counts.is_empty() {
                for vp in &pattern.vertex_patterns {
                    if let Some(label) = vp.label.as_deref() {
                        if let Some(vlc) = gs.vertex_label_counts.get(label) {
                            vertex_sel *= if nv > 0.0 { *vlc as f64 / nv } else { 1.0 };
                        }
                    }
                }
            }

            let estimate = nv.powf(k as f64) * density.powf(e as f64) * label_sel * vertex_sel;
            return nv.min(estimate).max(1.0);
        }

        n.min(n.powf(1.5))
    }

    pub(super) fn estimate_temporal_pattern_match(
        &self,
        pattern: &GraphPatternIR,
        temporal_filter: Option<&TemporalFilterIR>,
        n: f64,
    ) -> f64 {
        let k = pattern.vertex_patterns.len();
        let e = pattern.edge_patterns.len();

        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };
            let density = gs.edge_density();

            let mut label_sel = 1.0;
            for ep in &pattern.edge_patterns {
                label_sel *= gs.label_selectivity(ep.label.as_deref());
            }

            let mut estimate = nv.powf(k as f64) * density.powf(e as f64) * label_sel;
            estimate = nv.min(estimate).max(1.0);

            if let Some(tf) = temporal_filter {
                estimate *= self.temporal_selectivity(tf, gs);
            }
            return estimate;
        }

        let mut estimate = n.min(n.powf(1.5));
        if let (Some(tf), Some(gs)) = (temporal_filter, self.graph_stats.as_ref()) {
            estimate *= self.temporal_selectivity(tf, gs);
        }
        estimate
    }

    /// Estimate RPQ cardinality from graph statistics. `|R|` (NFA size) is
    /// approximated directly from the expression source by counting
    /// label-bearing tokens.
    pub(super) fn estimate_rpq(&self, rpq_source: &str, n: f64) -> f64 {
        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };
            let density = gs.edge_density();
            let r_size = rpq_label_count(rpq_source).max(1) as f64;
            let estimate = nv.powi(2) * r_size * density;
            return nv.min(estimate).max(1.0);
        }
        n.min(n.powf(1.5))
    }

    /// Estimate the spherical-cap tail probability for cosine similarity.
    /// Random unit vectors in `dimensions` coordinates have an asymptotically
    /// normal dot product with variance `1 / dimensions`.
    pub fn vector_selectivity(threshold: f64, dimensions: u32) -> f64 {
        if threshold <= -1.0 {
            return 1.0;
        }
        if threshold >= 1.0 {
            return 0.0;
        }
        let cosine = threshold.clamp(-1.0, 1.0);
        let z = cosine * f64::from(dimensions.max(1)).sqrt();
        normal_survival(z).clamp(0.0, 1.0)
    }

    pub(super) fn estimate_join_side(
        &self,
        side: &OperatorTree,
        stats: &IndexStats,
        _n: f64,
    ) -> f64 {
        self.estimate(side, stats)
    }

    /// Fixed-size random-walk estimate. `None` means no graph store is available
    /// or an index cannot be represented on the current target.
    ///
    /// This deterministic sample is a cost heuristic. It does not report a
    /// confidence interval and makes no distribution-free error guarantee.
    fn sample_graph_cardinality(
        &self,
        pattern: &GraphPatternIR,
        sample_size: usize,
    ) -> Option<f64> {
        let Some(store) = self.graph_store.as_ref() else {
            return None;
        };
        let vertex_ids = store.vertex_ids();
        if vertex_ids.is_empty() {
            return Some(0.0);
        }
        let k = pattern.vertex_patterns.len();
        if k == 0 {
            return Some(0.0);
        }

        let n = vertex_ids.len();
        let rng = XorShiftRng::new(0xDEAD_BEEF);
        let mut weighted_matches = 0.0;

        for _ in 0..sample_size {
            let start = vertex_ids[rng.bounded(n)?];
            let vp0 = &pattern.vertex_patterns[0];
            if !sample_vertex_matches(store.as_ref(), start, vp0) {
                continue;
            }

            let mut assignment: BTreeMap<String, u64> = BTreeMap::new();
            assignment.insert(vp0.variable.clone(), start);
            let mut valid = true;
            let mut path_weight = 1.0;

            for vi in 1..k {
                let vp = &pattern.vertex_patterns[vi];
                let mut neighbor_found = false;
                for ep in &pattern.edge_patterns {
                    if ep.target_var != vp.variable {
                        continue;
                    }
                    let Some(src_id) = assignment.get(&ep.source_var).copied() else {
                        continue;
                    };
                    let edges = store.outgoing_edges(src_id);
                    let mut candidates: Vec<u64> = Vec::new();
                    for edge in edges {
                        if let Some(label) = &ep.label {
                            if &edge.label != label {
                                continue;
                            }
                        }
                        if sample_vertex_matches(store.as_ref(), edge.target_id, vp) {
                            candidates.push(edge.target_id);
                        }
                    }
                    if !candidates.is_empty() {
                        path_weight *= candidates.len() as f64;
                        let picked = candidates[rng.bounded(candidates.len())?];
                        assignment.insert(vp.variable.clone(), picked);
                        neighbor_found = true;
                        break;
                    }
                }
                if !neighbor_found {
                    valid = false;
                    break;
                }
            }

            if valid && assignment.len() == k {
                weighted_matches += path_weight;
            }
        }

        let mean_matches_per_start = weighted_matches / sample_size as f64;
        Some(mean_matches_per_start * n as f64)
    }

    fn temporal_selectivity(&self, filter: &TemporalFilterIR, gs: &GraphStats) -> f64 {
        let (Some(min_ts), Some(max_ts)) = (gs.min_timestamp, gs.max_timestamp) else {
            return 1.0;
        };
        let total_range = max_ts - min_ts;
        if total_range <= 0.0 {
            return 1.0;
        }
        if filter.timestamp.is_some() {
            return (1.0 / total_range).min(1.0);
        }
        if let Some((lo, hi)) = filter.time_range {
            let span = hi - lo;
            return (span / total_range).min(1.0);
        }
        1.0
    }
}

fn sample_vertex_matches(
    store: &dyn GraphStoreSampler,
    vertex_id: u64,
    pattern: &uqa_operators::VertexPatternIR,
) -> bool {
    if let Some(label) = pattern.label.as_ref() {
        let expected = label.clone();
        let label_constraint: VertexConstraint = Arc::new(move |vertex| vertex.label == expected);
        if !store.vertex_satisfies(vertex_id, &label_constraint) {
            return false;
        }
    }
    pattern
        .constraints
        .iter()
        .all(|constraint| store.vertex_satisfies(vertex_id, constraint))
}

/// Abramowitz-Stegun 26.2.17 approximation of the standard-normal survival
/// function. The maximum absolute error is below 7.5e-8.
fn normal_survival(z: f64) -> f64 {
    let x = z.abs();
    let t = 1.0 / (1.0 + 0.231_641_9 * x);
    let polynomial = t
        * (0.319_381_530
            + t * (-0.356_563_782
                + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
    let upper = (-0.5 * x * x).exp() * polynomial / (2.0 * std::f64::consts::PI).sqrt();
    if z >= 0.0 {
        upper
    } else {
        1.0 - upper
    }
}
