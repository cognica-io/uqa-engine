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
//! `JoinOrderTree` retains the executable physical strategy selected by the
//! enumerator. Today relational equijoins are hash joins; the planner does not
//! pretend that a pre-existing index join is available when the engine cannot
//! execute one.

use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

use crate::cardinality::{AccessParadigm, ColumnStatistics};
use crate::cost_model::{CostEstimator, OperatorKind};
use crate::join_enumerator::JoinPlan;
use crate::join_graph::{JoinGraph, JoinGraphError, JoinGraphResult};

mod selection;

/// Invalid join inputs or failed admission while selecting their order.
#[derive(Debug, thiserror::Error)]
pub enum JoinOrderError {
    #[error(transparent)]
    Graph(#[from] JoinGraphError),
    #[error(transparent)]
    Production(#[from] ValueRetentionError),
}

/// Description of a base relation feeding a join-order search.
#[derive(Debug, Clone)]
pub struct JoinRelation {
    pub alias: String,
    pub cardinality: f64,
    pub column_stats: ColumnStatistics,
    /// Cost of producing this relation after local access predicates.
    pub access_cost: f64,
    /// Physical domain used by the relation-local access path.
    pub paradigm: AccessParadigm,
    /// Relation reference the engine resolves back to its backing
    /// data (e.g. a table id or scan handle).
    pub source_id: u64,
}

/// Selection observes existing relation metadata without requiring an owned alias or column map.
#[derive(Clone, Copy)]
pub(crate) struct JoinRelationRef<'a> {
    pub alias: &'a str,
    pub cardinality: f64,
    pub column_stats: &'a ColumnStatistics,
    pub access_cost: f64,
}

impl JoinRelation {
    fn as_join_relation(&self) -> JoinRelationRef<'_> {
        JoinRelationRef {
            alias: &self.alias,
            cardinality: self.cardinality,
            column_stats: &self.column_stats,
            access_cost: self.access_cost,
        }
    }
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

/// Determines an optimal join ordering using DPccp.
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
    ) -> JoinGraphResult<JoinOrderResult> {
        let selection::Selection { plan, aliases } = match self.select(
            relations.iter().map(JoinRelation::as_join_relation),
            &predicates,
            &ProductionControl::uncontrolled(),
        ) {
            Ok(selection) => selection,
            Err(JoinOrderError::Graph(error)) => return Err(error),
            Err(JoinOrderError::Production(error)) => {
                panic!("uncontrolled join-order allocation failed: {error}");
            }
        };
        let primary_alias = relations.first().map(|relation| relation.alias.clone());
        let mut predicate_lookup = BTreeMap::new();
        if relations.len() > 1 {
            for predicate in predicates {
                let left = aliases.index(&predicate.left_alias)?;
                let right = aliases.index(&predicate.right_alias)?;
                // Preserve the existing last predicate for each relation pair.
                predicate_lookup.insert((left.min(right), left.max(right)), predicate);
            }
        }
        let mut sources: [Option<JoinRelation>; 64] = std::array::from_fn(|_| None);
        for (index, relation) in relations.into_iter().enumerate() {
            sources[index] = Some(relation);
        }
        let tree = Self::materialize_plan(&plan, &mut sources, &mut predicate_lookup)?;
        Ok(JoinOrderResult {
            tree,
            primary_alias,
        })
    }

    /// Select an admitted join tree from borrowed metadata. Graph and search
    /// scratch release before return; the output retains only its boxed nodes.
    /// Relation statistics remain with their input owner and are never copied.
    pub fn select_with_control(
        &self,
        relations: &[JoinRelation],
        predicates: &[JoinPredicate],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<JoinPlan>, JoinOrderError> {
        self.select_relations_with_control(
            relations.iter().map(JoinRelation::as_join_relation),
            predicates,
            control,
        )
    }

    pub(crate) fn select_relations_with_control<'a>(
        &self,
        relations: impl ExactSizeIterator<Item = JoinRelationRef<'a>> + Clone,
        predicates: &[JoinPredicate],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<JoinPlan>, JoinOrderError> {
        self.select(relations, predicates, control)
            .map(|selection| selection.plan)
    }

    fn estimate_predicate_selectivity<'a>(
        relations: impl ExactSizeIterator<Item = JoinRelationRef<'a>> + Clone,
        left_idx: usize,
        right_idx: usize,
        left_field: &str,
        right_field: &str,
    ) -> f64 {
        let l_distinct = relations
            .clone()
            .nth(left_idx)
            .and_then(|r| r.column_stats.get(left_field))
            .map(|s| s.distinct_count.max(1));
        let r_distinct = relations
            .clone()
            .nth(right_idx)
            .and_then(|r| r.column_stats.get(right_field))
            .map(|s| s.distinct_count.max(1));
        match (l_distinct, r_distinct) {
            (Some(left), Some(right)) => 1.0 / left.max(right) as f64,
            (Some(distinct), None) | (None, Some(distinct)) => 1.0 / distinct as f64,
            (None, None) => crate::CardinalityEstimator::new().default_selectivity,
        }
    }

    fn materialize_plan(
        plan: &JoinPlan,
        relations: &mut [Option<JoinRelation>],
        predicates: &mut BTreeMap<(usize, usize), JoinPredicate>,
    ) -> JoinGraphResult<JoinOrderTree> {
        match (&plan.left, &plan.right) {
            (None, None) => {
                // Leaf: `relations` is a singleton bitmask of the
                // source relation index.
                let idx = usize::try_from(plan.relations.trailing_zeros()).map_err(|_| {
                    crate::join_graph::JoinGraphError::InvalidPlan {
                        detail: "join leaf index exceeds usize".into(),
                    }
                })?;
                relations
                    .get_mut(idx)
                    .and_then(Option::take)
                    .map(JoinOrderTree::Scan)
                    .ok_or_else(|| crate::join_graph::JoinGraphError::InvalidPlan {
                        detail: format!("leaf references relation index {idx}"),
                    })
            }
            (Some(left), Some(right)) => {
                let l_tree = Self::materialize_plan(left, relations, predicates)?;
                let r_tree = Self::materialize_plan(right, relations, predicates)?;
                let l_set = left.relations;
                let Some(edge) = plan.join_edge else {
                    return Ok(JoinOrderTree::Cross {
                        left: Box::new(l_tree),
                        right: Box::new(r_tree),
                    });
                };

                let edge_left_bit = usize::try_from(edge.left.trailing_zeros()).map_err(|_| {
                    crate::join_graph::JoinGraphError::InvalidPlan {
                        detail: "left join edge index exceeds usize".into(),
                    }
                })?;
                let edge_right_bit =
                    usize::try_from(edge.right.trailing_zeros()).map_err(|_| {
                        crate::join_graph::JoinGraphError::InvalidPlan {
                            detail: "right join edge index exceeds usize".into(),
                        }
                    })?;
                let left_in_l = (l_set & (1u64 << edge_left_bit)) != 0;
                let right_in_l = (l_set & (1u64 << edge_right_bit)) != 0;
                let key = (
                    edge_left_bit.min(edge_right_bit),
                    edge_left_bit.max(edge_right_bit),
                );
                let Some(pred) = predicates.remove(&key) else {
                    return Err(crate::join_graph::JoinGraphError::InvalidPlan {
                        detail: format!(
                            "join edge between relation indices {edge_left_bit} and {edge_right_bit} has no predicate"
                        ),
                    });
                };

                // Orient condition fields: if the plan put the edge's
                // RIGHT relation on the LEFT side, swap the fields so
                // left_field corresponds to the actual left side.
                let condition = if !left_in_l && right_in_l {
                    JoinCondition {
                        left_field: pred.right_field,
                        right_field: pred.left_field,
                    }
                } else {
                    JoinCondition {
                        left_field: pred.left_field,
                        right_field: pred.right_field,
                    }
                };

                let algorithm = match plan.kind {
                    Some(OperatorKind::HashJoinInner) => JoinAlgorithm::Hash,
                    Some(kind) => {
                        return Err(crate::join_graph::JoinGraphError::InvalidPlan {
                            detail: format!(
                                "DPccp selected non-executable equijoin strategy {kind:?}"
                            ),
                        });
                    }
                    None => {
                        return Err(crate::join_graph::JoinGraphError::InvalidPlan {
                            detail: "DPccp equijoin node has no physical strategy".into(),
                        });
                    }
                };

                Ok(JoinOrderTree::Inner {
                    algorithm,
                    condition,
                    left: Box::new(l_tree),
                    right: Box::new(r_tree),
                })
            }
            (Some(_), None) | (None, Some(_)) => {
                Err(crate::join_graph::JoinGraphError::InvalidPlan {
                    detail: "join node contains exactly one child".into(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColumnStats;
    use uqa_core::Value;

    mod memory;

    fn rel(alias: &str, card: f64, src: u64) -> JoinRelation {
        JoinRelation {
            alias: alias.into(),
            cardinality: card,
            column_stats: ColumnStatistics::default(),
            access_cost: card,
            paradigm: AccessParadigm::Relational,
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
            column_stats: cs.into(),
            access_cost: card,
            paradigm: AccessParadigm::Relational,
            source_id: src,
        }
    }

    fn assert_hash_join_tree(tree: &JoinOrderTree) {
        match tree {
            JoinOrderTree::Inner {
                algorithm,
                left,
                right,
                ..
            } => {
                assert_eq!(*algorithm, JoinAlgorithm::Hash);
                assert_hash_join_tree(left);
                assert_hash_join_tree(right);
            }
            JoinOrderTree::Cross { left, right } => {
                assert_hash_join_tree(left);
                assert_hash_join_tree(right);
            }
            JoinOrderTree::Scan(_) => {}
        }
    }

    #[test]
    fn single_relation_returns_scan() {
        let opt = JoinOrderOptimizer::new();
        let result = opt.optimize(vec![rel("a", 100.0, 1)], vec![]).unwrap();
        assert!(matches!(result.tree, JoinOrderTree::Scan(_)));
        assert_eq!(result.primary_alias.as_deref(), Some("a"));
    }

    #[test]
    fn three_chain_uses_executable_hash_strategies() {
        let opt = JoinOrderOptimizer::new();
        let result = opt
            .optimize(
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
            )
            .unwrap();
        assert_hash_join_tree(&result.tree);
        assert_eq!(result.primary_alias.as_deref(), Some("small"));
    }

    #[test]
    fn cross_join_when_no_predicate() {
        let opt = JoinOrderOptimizer::new();
        let result = opt
            .optimize(vec![rel("a", 50.0, 1), rel("b", 50.0, 2)], vec![])
            .unwrap();
        match result.tree {
            JoinOrderTree::Inner { .. } => panic!("expected cross join, got inner"),
            JoinOrderTree::Scan(_) => panic!("expected cross join, got scan"),
            JoinOrderTree::Cross { .. } => {}
        }
    }

    #[test]
    fn predicate_selectivity_uses_max_distinct() {
        let relations = [
            rel_with_stats("a", 1000.0, 1, "x", 100),
            rel_with_stats("b", 1000.0, 2, "y", 50),
        ];
        let s = JoinOrderOptimizer::estimate_predicate_selectivity(
            relations.iter().map(JoinRelation::as_join_relation),
            0,
            1,
            "x",
            "y",
        );
        assert!((s - 0.01).abs() < 1e-9);
        let _ = Value::Int(0);
    }

    #[test]
    fn predicate_selectivity_uses_the_configured_fallback_without_statistics() {
        let relations = [rel("a", 1000.0, 1), rel("b", 1000.0, 2)];
        let selectivity = JoinOrderOptimizer::estimate_predicate_selectivity(
            relations.iter().map(JoinRelation::as_join_relation),
            0,
            1,
            "x",
            "y",
        );
        assert_eq!(
            selectivity,
            crate::CardinalityEstimator::new().default_selectivity
        );
    }

    #[test]
    fn invalid_join_inputs_are_reported_instead_of_silently_rewritten() {
        let optimizer = JoinOrderOptimizer::new();
        assert!(matches!(
            optimizer.optimize(Vec::new(), Vec::new()),
            Err(crate::join_graph::JoinGraphError::EmptyGraph)
        ));
        assert!(matches!(
            optimizer.optimize(vec![rel("a", 1.0, 1), rel("a", 2.0, 2)], Vec::new()),
            Err(crate::join_graph::JoinGraphError::DuplicateAlias { .. })
        ));
        assert!(matches!(
            optimizer.optimize(
                vec![rel("a", 1.0, 1), rel("b", 2.0, 2)],
                vec![JoinPredicate {
                    left_alias: "a".into(),
                    right_alias: "missing".into(),
                    left_field: "id".into(),
                    right_field: "id".into(),
                }],
            ),
            Err(crate::join_graph::JoinGraphError::UnknownAlias { .. })
        ));
        assert!(matches!(
            optimizer.optimize(vec![rel("bad", f64::NAN, 1)], Vec::new()),
            Err(crate::join_graph::JoinGraphError::InvalidCardinality { .. })
        ));
    }
}
