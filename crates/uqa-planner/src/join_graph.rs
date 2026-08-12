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
//! suffices for every shape the UQA SQL compiler can build.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum JoinGraphError {
    #[error("join-order optimization requires at least one relation")]
    EmptyGraph,
    #[error("JoinGraph supports at most 64 relations; cannot add `{name}` at index {index}")]
    TooManyRelations { name: String, index: usize },
    #[error(
        "join edge references relation index {index}, but the graph contains {relation_count} relations"
    )]
    UnknownRelation { index: usize, relation_count: usize },
    #[error("relation `{name}` has invalid cardinality estimate {rows}; expected a finite non-negative value")]
    InvalidCardinality { name: String, rows: f64 },
    #[error(
        "relation `{name}` has invalid access cost {cost}; expected a finite non-negative value"
    )]
    InvalidAccessCost { name: String, cost: f64 },
    #[error("join selectivity must be finite and between 0 and 1, got {selectivity}")]
    InvalidSelectivity { selectivity: f64 },
    #[error("duplicate join relation alias `{alias}`")]
    DuplicateAlias { alias: String },
    #[error("join predicate references unknown relation alias `{alias}`")]
    UnknownAlias { alias: String },
    #[error("invalid join plan: {detail}")]
    InvalidPlan { detail: String },
}

pub type JoinGraphResult<T> = Result<T, JoinGraphError>;

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
    pub(crate) relations: Vec<String>,
    /// Per-relation row count estimate.
    pub(crate) cardinalities: Vec<f64>,
    /// Cost of producing each base relation after its local access predicates.
    pub(crate) access_costs: Vec<f64>,
    /// Connecting edges. Order is irrelevant; the enumerator probes
    /// them by bitmask membership.
    pub(crate) edges: Vec<JoinEdge>,
}

impl JoinGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_relation(&mut self, name: impl Into<String>, rows: f64) -> JoinGraphResult<usize> {
        self.add_relation_with_cost(name, rows, rows)
    }

    pub fn add_relation_with_cost(
        &mut self,
        name: impl Into<String>,
        rows: f64,
        access_cost: f64,
    ) -> JoinGraphResult<usize> {
        let idx = self.relations.len();
        let name = name.into();
        if idx >= 64 {
            return Err(JoinGraphError::TooManyRelations { name, index: idx });
        }
        if !rows.is_finite() || rows < 0.0 {
            return Err(JoinGraphError::InvalidCardinality { name, rows });
        }
        if !access_cost.is_finite() || access_cost < 0.0 {
            return Err(JoinGraphError::InvalidAccessCost {
                name,
                cost: access_cost,
            });
        }
        self.relations.push(name);
        self.cardinalities.push(rows);
        self.access_costs.push(access_cost);
        Ok(idx)
    }

    pub fn add_edge(
        &mut self,
        left_idx: usize,
        right_idx: usize,
        selectivity: f64,
    ) -> JoinGraphResult<()> {
        for index in [left_idx, right_idx] {
            if index >= self.relations.len() {
                return Err(JoinGraphError::UnknownRelation {
                    index,
                    relation_count: self.relations.len(),
                });
            }
        }
        if !selectivity.is_finite() || !(0.0..=1.0).contains(&selectivity) {
            return Err(JoinGraphError::InvalidSelectivity { selectivity });
        }
        self.edges.push(JoinEdge {
            left: 1u64 << left_idx,
            right: 1u64 << right_idx,
            selectivity,
        });
        Ok(())
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
        match self.relations.len() {
            0 => 0,
            64 => u64::MAX,
            count => (1u64 << count) - 1,
        }
    }

    /// Indices of every relation directly connected to `node` by any edge.
    /// Each call walks the edge list once: `O(|edges|)`.
    pub fn neighbors(&self, node: usize) -> Vec<usize> {
        if node >= self.relations.len() {
            return Vec::new();
        }
        let mark = 1u64 << node;
        let mut out: Vec<usize> = Vec::new();
        let mut seen: u64 = 0;
        for edge in &self.edges {
            let other = if edge.left == mark {
                edge.right
            } else if edge.right == mark {
                edge.left
            } else {
                continue;
            };
            if other == 0 {
                continue;
            }
            // `other` is a singleton bitmask of the other side.
            let Ok(idx) = usize::try_from(other.trailing_zeros()) else {
                continue;
            };
            if seen & (1u64 << idx) != 0 {
                continue;
            }
            seen |= 1u64 << idx;
            out.push(idx);
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_set_covers_every_relation() {
        let mut g = JoinGraph::new();
        for i in 0..3 {
            g.add_relation(format!("t{i}"), 100.0).unwrap();
        }
        assert_eq!(g.full_set(), 0b111);
    }

    #[test]
    fn edges_between_finds_predicates_across_partition() {
        let mut g = JoinGraph::new();
        let a = g.add_relation("a", 100.0).unwrap();
        let b = g.add_relation("b", 100.0).unwrap();
        let c = g.add_relation("c", 100.0).unwrap();
        g.add_edge(a, b, 0.01).unwrap();
        g.add_edge(b, c, 0.01).unwrap();
        let s1 = 1u64 << a;
        let s2 = (1u64 << b) | (1u64 << c);
        let between = g.edges_between(s1, s2);
        assert_eq!(between.len(), 1);
    }

    #[test]
    fn capacity_and_edge_errors_are_returned_without_panicking() {
        let mut graph = JoinGraph::new();
        for index in 0..64 {
            graph.add_relation(format!("t{index}"), 1.0).unwrap();
        }
        assert_eq!(graph.full_set(), u64::MAX);
        assert!(matches!(
            graph.add_relation("overflow", 1.0),
            Err(JoinGraphError::TooManyRelations { index: 64, .. })
        ));
        assert!(matches!(
            graph.add_edge(0, 64, 1.0),
            Err(JoinGraphError::UnknownRelation { index: 64, .. })
        ));
        assert!(graph.neighbors(64).is_empty());
    }

    #[test]
    fn rejects_non_finite_cost_inputs_before_enumeration() {
        let mut graph = JoinGraph::new();
        assert!(matches!(
            graph.add_relation("bad", f64::NAN),
            Err(JoinGraphError::InvalidCardinality { .. })
        ));
        assert!(matches!(
            graph.add_relation_with_cost("bad_cost", 1.0, f64::INFINITY),
            Err(JoinGraphError::InvalidAccessCost { .. })
        ));
        let left = graph.add_relation("left", 1.0).unwrap();
        let right = graph.add_relation("right", 1.0).unwrap();
        for selectivity in [f64::NAN, -0.1, 1.1] {
            assert!(matches!(
                graph.add_edge(left, right, selectivity),
                Err(JoinGraphError::InvalidSelectivity { .. })
            ));
        }
    }
}
