//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core `OperatorTree` cardinality dispatch and intersection damping.

use super::{
    column_entropy, entropy_cardinality_lower_bound, mutual_information_estimate,
    CardinalityEstimator, GraphStoreSampler, IndexStats, OperatorTree,
};

impl CardinalityEstimator {
    /// Estimate the cardinality of `op` against an inverted index described
    /// by `stats`.
    pub fn estimate(&self, op: &OperatorTree, stats: &IndexStats) -> f64 {
        let n = if stats.total_docs > 0 {
            stats.total_docs as f64
        } else {
            1.0
        };

        match op {
            OperatorTree::Empty => 0.0,
            OperatorTree::Term { query, field, .. } => {
                let field_name = field.as_deref().unwrap_or("_default");
                stats.doc_freq(field_name, query) as f64
            }
            OperatorTree::VectorSimilarity { threshold, .. } => {
                n * Self::vector_selectivity(*threshold)
            }
            OperatorTree::KNN { k, .. } => *k as f64,
            OperatorTree::Filter {
                field, predicate, ..
            } => n * self.filter_selectivity(field, predicate, n),
            OperatorTree::Score { source, .. } => self.estimate(source, stats),
            OperatorTree::BayesianScore { source, .. } => self.estimate(source, stats),
            OperatorTree::Intersect(ops) => self.estimate_intersect(ops, stats, n),
            OperatorTree::Union(ops) => {
                let child_cards: f64 = ops.iter().map(|o| self.estimate(o, stats)).sum();
                n.min(child_cards)
            }
            OperatorTree::Complement(inner) => {
                let inner_card = self.estimate(inner, stats);
                (n - inner_card).max(0.0)
            }
            _ => self.estimate_cross_paradigm(op, stats, n),
        }
    }

    /// Backward-compat alias used by [`crate::query_optimizer`]. Builds
    /// an [`IndexStats`] with `total_docs = row_count.unwrap_or(1000)`
    /// and routes through [`Self::estimate`].
    pub fn estimate_operator(&self, op: &OperatorTree, row_count: Option<u64>) -> f64 {
        let stats = IndexStats::new(row_count.unwrap_or(1_000));
        self.estimate(op, &stats)
    }

    fn estimate_intersect(&self, ops: &[OperatorTree], stats: &IndexStats, n: f64) -> f64 {
        let mut child_cards: Vec<f64> = ops.iter().map(|o| self.estimate(o, stats)).collect();
        if child_cards.is_empty() {
            return 0.0;
        }
        child_cards.sort_by(f64::total_cmp);

        let damping = self.intersection_damping(ops);
        let mut result = child_cards[0];
        for card in &child_cards[1..] {
            let sel = if n > 0.0 { card / n } else { 1.0 };
            result *= sel.powf(damping);
        }

        // Apply entropy-based lower bound (Paper 1, Section 7) when
        // column stats are available.
        if !self.column_stats.is_empty() {
            let mut entropies: Vec<f64> = Vec::new();
            for op_item in ops {
                if let OperatorTree::Filter { field, .. } = op_item {
                    if let Some(cs) = self.column_stats.get(field) {
                        entropies.push(column_entropy(cs));
                    }
                }
            }
            if !entropies.is_empty() {
                let lb = entropy_cardinality_lower_bound(n, &entropies);
                result = result.max(lb);
            }
        }

        result.max(1.0)
    }

    /// Choose a damping exponent based on predicate correlation.
    fn intersection_damping(&self, ops: &[OperatorTree]) -> f64 {
        let fields: Vec<&str> = ops
            .iter()
            .filter_map(|o| match o {
                OperatorTree::Filter { field, .. } => Some(field.as_str()),
                _ => None,
            })
            .collect();

        if fields.len() < 2 {
            return 0.5;
        }

        let unique: std::collections::BTreeSet<&str> = fields.iter().copied().collect();
        if unique.len() == 1 {
            return 0.1;
        }

        if !self.column_stats.is_empty() && fields.len() >= 2 {
            let cs_a = self.column_stats.get(fields[0]);
            let cs_b = self.column_stats.get(fields[1]);
            if let (Some(a), Some(b)) = (cs_a, cs_b) {
                let mi = mutual_information_estimate(a, b, 0.1);
                if mi > 1.0 {
                    return 0.2;
                }
                if mi > 0.5 {
                    return 0.3;
                }
            }
        }

        0.5
    }
}
