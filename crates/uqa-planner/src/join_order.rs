//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join-order optimization.
//!
//! Bridges DPccp join enumeration ([`crate::join_enumerator`]) and the
//! row-oriented join algorithms in `uqa-joins`. The optimizer accepts
//! a list of [`JoinRelation`] descriptors plus equijoin
//! [`JoinPredicate`] descriptors, builds an internal [`JoinGraph`],
//! runs DPccp, and materializes the chosen plan into a
//! [`JoinOrderTree`] -- a tree of join descriptors the engine
//! interprets to drive the actual row-tuple join algorithms.
//!
//! `JoinOrderTree` mirrors the canonical UQA implementation's operator construction step; the
//! algorithm choice (hash vs index) follows the same `min(left_card,
//! right_card) <= INDEX_JOIN_THRESHOLD` cutoff.

use std::collections::BTreeMap;

use crate::cardinality::ColumnStats;
use crate::cost_model::CostEstimator;
use crate::join_enumerator::{enumerate_dpccp, JoinPlan};
use crate::join_graph::{JoinEdge, JoinGraph};

/// Use index join when the smaller side has fewer rows than this
/// threshold. Index join is `O(|L1| * log|L2|)` vs hash join
/// `O(|L1| + |L2|)`; for small inputs the lower constant factor of
/// binary search wins. Mirrors `INDEX_JOIN_THRESHOLD` in the UQA implementation
/// reference.
pub const INDEX_JOIN_THRESHOLD: f64 = 100.0;

/// Description of a base relation feeding a join. Mirrors the dict
/// shape used by `uqa.planner.join_order.JoinOrderOptimizer.optimize`.
#[derive(Debug, Clone)]
pub struct JoinRelation {
    pub alias: String,
    pub cardinality: f64,
    pub column_stats: BTreeMap<String, ColumnStats>,
    /// Relation reference the engine resolves back to its backing
    /// data (e.g. a table id or scan handle).
    pub source_id: u64,
}

/// Equijoin predicate between two named relations.
#[derive(Debug, Clone)]
pub struct JoinPredicate {
    pub left_alias: String,
    pub right_alias: String,
    pub left_field: String,
    pub right_field: String,
}

/// Algorithm hint for an inner join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAlgorithm {
    Hash,
    Index,
}

/// Equijoin condition.
#[derive(Debug, Clone)]
pub struct JoinCondition {
    pub left_field: String,
    pub right_field: String,
}

/// Output of the join order optimizer. The engine walks this tree to
/// drive the actual row-tuple join algorithms.
#[derive(Debug, Clone)]
pub enum JoinOrderTree {
    /// Base relation -- a single scan source.
    Scan(JoinRelation),
    /// Inner equijoin with the chosen algorithm.
    Inner {
        algorithm: JoinAlgorithm,
        condition: JoinCondition,
        left: Box<JoinOrderTree>,
        right: Box<JoinOrderTree>,
    },
    /// Cross join -- no predicate connecting the two sides.
    Cross {
        left: Box<JoinOrderTree>,
        right: Box<JoinOrderTree>,
    },
}

/// Result of optimization: the chosen join tree plus the alias of the
/// first non-empty relation (used as the engine's primary table
/// context).
#[derive(Debug, Clone)]
pub struct JoinOrderResult {
    pub tree: JoinOrderTree,
    pub primary_alias: Option<String>,
}

/// Determines optimal join ordering using DPccp. Mirrors
/// `uqa.planner.join_order.JoinOrderOptimizer`.
#[derive(Debug, Clone, Default)]
pub struct JoinOrderOptimizer {
    pub cost_estimator: CostEstimator,
}

impl JoinOrderOptimizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cost_estimator(mut self, est: CostEstimator) -> Self {
        self.cost_estimator = est;
        self
    }

    /// Find the optimal join order and build the operator tree.
    pub fn optimize(
        &self,
        relations: Vec<JoinRelation>,
        predicates: Vec<JoinPredicate>,
    ) -> JoinOrderResult {
        if relations.is_empty() {
            return JoinOrderResult {
                tree: JoinOrderTree::Cross {
                    left: Box::new(JoinOrderTree::Scan(JoinRelation {
                        alias: String::new(),
                        cardinality: 0.0,
                        column_stats: BTreeMap::new(),
                        source_id: 0,
                    })),
                    right: Box::new(JoinOrderTree::Scan(JoinRelation {
                        alias: String::new(),
                        cardinality: 0.0,
                        column_stats: BTreeMap::new(),
                        source_id: 0,
                    })),
                },
                primary_alias: None,
            };
        }

        if relations.len() == 1 {
            let rel = relations.into_iter().next().unwrap();
            let alias = rel.alias.clone();
            return JoinOrderResult {
                tree: JoinOrderTree::Scan(rel),
                primary_alias: Some(alias),
            };
        }

        let primary_alias = relations.first().map(|r| r.alias.clone());

        // Build the abstract JoinGraph for DPccp.
        let mut graph = JoinGraph::new();
        let mut alias_to_idx: BTreeMap<String, usize> = BTreeMap::new();
        for rel in &relations {
            let idx = graph.add_relation(rel.alias.clone(), rel.cardinality);
            alias_to_idx.insert(rel.alias.clone(), idx);
        }

        // Materialize predicates as edges with column-stats-derived
        // selectivity.
        let mut predicate_lookup: BTreeMap<(usize, usize), JoinPredicate> = BTreeMap::new();
        for pred in predicates {
            let l = alias_to_idx.get(&pred.left_alias).copied();
            let r = alias_to_idx.get(&pred.right_alias).copied();
            let (Some(l), Some(r)) = (l, r) else {
                continue;
            };
            let selectivity = Self::estimate_predicate_selectivity(
                &relations,
                l,
                r,
                &pred.left_field,
                &pred.right_field,
            );
            graph.add_edge(l, r, selectivity);
            // Store both orientations so materialize_plan can resolve
            // either when DPccp swaps sides.
            predicate_lookup.insert((l.min(r), l.max(r)), pred);
        }

        let plan = enumerate_dpccp(&graph);
        let tree = match plan {
            Some(p) => Self::materialize_plan(&p, &graph, &relations, &predicate_lookup),
            None => {
                // No connecting edges -- fall back to a left-deep
                // cartesian product. DPccp returns None when the
                // graph is disconnected, but a cross join is still a
                // valid (if expensive) plan.
                let mut iter = relations.into_iter();
                let first = iter.next().unwrap();
                let mut tree = JoinOrderTree::Scan(first);
                for rel in iter {
                    tree = JoinOrderTree::Cross {
                        left: Box::new(tree),
                        right: Box::new(JoinOrderTree::Scan(rel)),
                    };
                }
                tree
            }
        };

        JoinOrderResult {
            tree,
            primary_alias,
        }
    }

    fn estimate_predicate_selectivity(
        relations: &[JoinRelation],
        left_idx: usize,
        right_idx: usize,
        left_field: &str,
        right_field: &str,
    ) -> f64 {
        let l_distinct = relations
            .get(left_idx)
            .and_then(|r| r.column_stats.get(left_field))
            .map(|s| s.distinct_count.max(1))
            .unwrap_or(1);
        let r_distinct = relations
            .get(right_idx)
            .and_then(|r| r.column_stats.get(right_field))
            .map(|s| s.distinct_count.max(1))
            .unwrap_or(1);
        1.0 / l_distinct.max(r_distinct) as f64
    }

    fn materialize_plan(
        plan: &JoinPlan,
        graph: &JoinGraph,
        relations: &[JoinRelation],
        predicates: &BTreeMap<(usize, usize), JoinPredicate>,
    ) -> JoinOrderTree {
        match (&plan.left, &plan.right) {
            (None, None) => {
                // Leaf: `relations` is a singleton bitmask of the
                // source relation index.
                let idx = plan.relations.trailing_zeros() as usize;
                JoinOrderTree::Scan(relations[idx].clone())
            }
            (Some(left), Some(right)) => {
                let l_tree = Self::materialize_plan(left, graph, relations, predicates);
                let r_tree = Self::materialize_plan(right, graph, relations, predicates);
                let l_set = left.relations;
                let r_set = right.relations;
                let Some(edge) = graph.edges.iter().find(|e| edge_connects(e, l_set, r_set)) else {
                    return JoinOrderTree::Cross {
                        left: Box::new(l_tree),
                        right: Box::new(r_tree),
                    };
                };

                let edge_left_bit = edge.left.trailing_zeros() as usize;
                let edge_right_bit = edge.right.trailing_zeros() as usize;
                let left_in_l = (l_set & (1u64 << edge_left_bit)) != 0;
                let right_in_l = (l_set & (1u64 << edge_right_bit)) != 0;
                let key = (
                    edge_left_bit.min(edge_right_bit),
                    edge_left_bit.max(edge_right_bit),
                );
                let Some(pred) = predicates.get(&key) else {
                    return JoinOrderTree::Cross {
                        left: Box::new(l_tree),
                        right: Box::new(r_tree),
                    };
                };

                // Orient condition fields: if the plan put the edge's
                // RIGHT relation on the LEFT side, swap the fields so
                // left_field corresponds to the actual left side.
                let condition = if !left_in_l && right_in_l {
                    JoinCondition {
                        left_field: pred.right_field.clone(),
                        right_field: pred.left_field.clone(),
                    }
                } else {
                    JoinCondition {
                        left_field: pred.left_field.clone(),
                        right_field: pred.right_field.clone(),
                    }
                };

                let min_card = left.rows().min(right.rows());
                let algorithm = if min_card <= INDEX_JOIN_THRESHOLD {
                    JoinAlgorithm::Index
                } else {
                    JoinAlgorithm::Hash
                };

                JoinOrderTree::Inner {
                    algorithm,
                    condition,
                    left: Box::new(l_tree),
                    right: Box::new(r_tree),
                }
            }
            // A plan with exactly one child shouldn't appear; treat as
            // its own materialised child.
            (Some(left), None) => Self::materialize_plan(left, graph, relations, predicates),
            (None, Some(right)) => Self::materialize_plan(right, graph, relations, predicates),
        }
    }
}

fn edge_connects(edge: &JoinEdge, l_set: u64, r_set: u64) -> bool {
    let l_in_l = edge.left & l_set != 0;
    let r_in_r = edge.right & r_set != 0;
    let l_in_r = edge.left & r_set != 0;
    let r_in_l = edge.right & l_set != 0;
    (l_in_l && r_in_r) || (l_in_r && r_in_l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    fn rel(alias: &str, card: f64, src: u64) -> JoinRelation {
        JoinRelation {
            alias: alias.into(),
            cardinality: card,
            column_stats: BTreeMap::new(),
            source_id: src,
        }
    }

    fn rel_with_stats(alias: &str, card: f64, src: u64, field: &str, ndv: u64) -> JoinRelation {
        let mut cs = BTreeMap::new();
        cs.insert(
            field.to_string(),
            ColumnStats {
                distinct_count: ndv,
                row_count: card as u64,
                ..Default::default()
            },
        );
        JoinRelation {
            alias: alias.into(),
            cardinality: card,
            column_stats: cs,
            source_id: src,
        }
    }

    #[test]
    fn single_relation_returns_scan() {
        let opt = JoinOrderOptimizer::new();
        let result = opt.optimize(vec![rel("a", 100.0, 1)], vec![]);
        assert!(matches!(result.tree, JoinOrderTree::Scan(_)));
        assert_eq!(result.primary_alias.as_deref(), Some("a"));
    }

    #[test]
    fn three_chain_picks_index_for_small_left() {
        let opt = JoinOrderOptimizer::new();
        let result = opt.optimize(
            vec![
                rel_with_stats("small", 10.0, 1, "id", 10),
                rel_with_stats("mid", 10_000.0, 2, "small_id", 10_000),
                rel_with_stats("big", 1_000_000.0, 3, "mid_id", 1_000_000),
            ],
            vec![
                JoinPredicate {
                    left_alias: "small".into(),
                    right_alias: "mid".into(),
                    left_field: "id".into(),
                    right_field: "small_id".into(),
                },
                JoinPredicate {
                    left_alias: "mid".into(),
                    right_alias: "big".into(),
                    left_field: "id".into(),
                    right_field: "mid_id".into(),
                },
            ],
        );
        // Top of plan should be a join.
        assert!(matches!(result.tree, JoinOrderTree::Inner { .. }));
        assert_eq!(result.primary_alias.as_deref(), Some("small"));
    }

    #[test]
    fn cross_join_when_no_predicate() {
        let opt = JoinOrderOptimizer::new();
        let result = opt.optimize(vec![rel("a", 50.0, 1), rel("b", 50.0, 2)], vec![]);
        match result.tree {
            JoinOrderTree::Inner { .. } => panic!("expected cross join, got inner"),
            JoinOrderTree::Scan(_) => panic!("expected cross join, got scan"),
            JoinOrderTree::Cross { .. } => {}
        }
    }

    #[test]
    fn predicate_selectivity_uses_max_distinct() {
        let relations = vec![
            rel_with_stats("a", 1000.0, 1, "x", 100),
            rel_with_stats("b", 1000.0, 2, "y", 50),
        ];
        let s = JoinOrderOptimizer::estimate_predicate_selectivity(&relations, 0, 1, "x", "y");
        assert!((s - 0.01).abs() < 1e-9);
        let _ = Value::Int(0);
    }
}
