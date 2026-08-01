//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Information-theoretic correlation and cardinality helpers.

use super::ColumnStats;

// ---------------------------------------------------------------------
// Information-theoretic helpers (Paper 1, Section 7).
// ---------------------------------------------------------------------

/// Estimate column entropy from MCV frequencies, equi-depth histogram,
/// or distinct count. Mirrors `_column_entropy`.
pub fn column_entropy(cs: &ColumnStats) -> f64 {
    let ndv = cs.distinct_count;
    if ndv <= 1 {
        return 0.0;
    }

    if !cs.mcv_frequencies.is_empty() {
        let mut entropy = 0.0;
        let sum: f64 = cs.mcv_frequencies.iter().sum();
        let remaining = (1.0 - sum).max(0.0);
        for freq in &cs.mcv_frequencies {
            if *freq > 0.0 {
                entropy -= freq * freq.log2();
            }
        }
        let remaining_ndv = ndv
            .saturating_sub(u64::try_from(cs.mcv_frequencies.len()).unwrap_or(u64::MAX))
            .max(1);
        if remaining > 0.0 && remaining_ndv > 0 {
            let p = remaining / remaining_ndv as f64;
            if p > 0.0 {
                entropy -= remaining * p.log2();
            }
        }
        return entropy.max(0.0);
    }

    if cs.histogram.len() > 1 {
        let num_buckets = (cs.histogram.len() - 1) as f64;
        if cs.row_count > 0 && num_buckets > 0.0 {
            let p = 1.0 / num_buckets;
            let entropy = -num_buckets * p * p.log2();
            return entropy.max(0.0);
        }
    }

    (ndv as f64).log2()
}

/// Estimate mutual information `I(X;Y) = H(X) + H(Y) - H(X,Y)`. Mirrors
/// `_mutual_information_estimate`.
pub fn mutual_information_estimate(
    cs_x: &ColumnStats,
    cs_y: &ColumnStats,
    joint_selectivity: f64,
) -> f64 {
    let h_x = column_entropy(cs_x);
    let h_y = column_entropy(cs_y);
    if joint_selectivity <= 0.0 {
        return 0.0;
    }
    let ndv_x = cs_x.distinct_count.max(1);
    let ndv_y = cs_y.distinct_count.max(1);
    let independent = (ndv_x as f64) * (ndv_y as f64);
    let effective = (independent * joint_selectivity).max(1.0);
    let h_joint = effective.max(1.0).log2();
    (h_x + h_y - h_joint).max(0.0)
}

/// Information-theoretic lower bound on intersection cardinality.
pub fn entropy_cardinality_lower_bound(n: f64, entropies: &[f64]) -> f64 {
    if entropies.is_empty() || n <= 0.0 {
        return 1.0;
    }
    let total: f64 = entropies.iter().sum();
    let lb = n * 2.0_f64.powf(-total);
    lb.max(1.0)
}
