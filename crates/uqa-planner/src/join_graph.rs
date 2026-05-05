//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join graph: nodes are individual relations (a single table or a
//! materialised CTE / subquery output), edges are equijoin predicates.
//! The DPccp enumerator walks this graph to pick a join order.
//!
//! The graph is dense by design (`u64` bitmask side sets) so DP cache
//! lookups can use bitmasks directly. With 64 relations max this
//! suffices for every shape the Python compiler can build.

#[derive(Debug, Clone)]
pub struct JoinEdge {
    /// Bitmask of relations on the left side of the equijoin.
    pub left: u64,
    /// Bitmask of relations on the right side of the equijoin.
    pub right: u64,
    /// Selectivity of the join predicate (1 / max(distinct_left,
    /// distinct_right) for an equijoin).
    pub selectivity: f64,
}

#[derive(Debug, Clone, Default)]
pub struct JoinGraph {
    /// Relation labels in declaration order. The bit index of a
    /// relation is its position in this vector.
    pub relations: Vec<String>,
    /// Per-relation row count estimate.
    pub cardinalities: Vec<f64>,
    /// Connecting edges. Order is irrelevant; the enumerator probes
    /// them by bitmask membership.
    pub edges: Vec<JoinEdge>,
}

impl JoinGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_relation(&mut self, name: impl Into<String>, rows: f64) -> usize {
        let idx = self.relations.len();
        if idx >= 64 {
            panic!(
                "JoinGraph supports up to 64 relations; got `{}` when adding {}",
                self.relations.len(),
                name.into()
            );
        }
        self.relations.push(name.into());
        self.cardinalities.push(rows);
        idx
    }

    pub fn add_edge(&mut self, left_idx: usize, right_idx: usize, selectivity: f64) {
        self.edges.push(JoinEdge {
            left: 1u64 << left_idx,
            right: 1u64 << right_idx,
            selectivity,
        });
    }

    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }

    /// All edges connecting a left subset to a right subset of the
    /// graph. An edge is "between" `(s1, s2)` when one endpoint is in
    /// `s1` and the other is in `s2`.
    pub fn edges_between(&self, s1: u64, s2: u64) -> Vec<&JoinEdge> {
        self.edges
            .iter()
            .filter(|e| {
                let l_in_1 = e.left & s1 != 0;
                let r_in_2 = e.right & s2 != 0;
                let l_in_2 = e.left & s2 != 0;
                let r_in_1 = e.right & s1 != 0;
                (l_in_1 && r_in_2) || (l_in_2 && r_in_1)
            })
            .collect()
    }

    pub fn full_set(&self) -> u64 {
        if self.relations.is_empty() {
            0
        } else {
            (1u64 << self.relations.len()) - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_set_covers_every_relation() {
        let mut g = JoinGraph::new();
        for i in 0..3 {
            g.add_relation(format!("t{i}"), 100.0);
        }
        assert_eq!(g.full_set(), 0b111);
    }

    #[test]
    fn edges_between_finds_predicates_across_partition() {
        let mut g = JoinGraph::new();
        let a = g.add_relation("a", 100.0);
        let b = g.add_relation("b", 100.0);
        let c = g.add_relation("c", 100.0);
        g.add_edge(a, b, 0.01);
        g.add_edge(b, c, 0.01);
        let s1 = 1u64 << a;
        let s2 = (1u64 << b) | (1u64 << c);
        let between = g.edges_between(s1, s2);
        assert_eq!(between.len(), 1);
    }
}
