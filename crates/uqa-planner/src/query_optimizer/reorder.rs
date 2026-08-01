//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cost-based filter, intersection, and fusion-signal ordering.

use uqa_operators::OperatorTree;

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // 2. Filter pushdown into Intersect
    // ---------------------------------------------------------------

    pub(super) fn push_filters_down(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            if let OperatorTree::Intersect(operands) = *s {
                let mut new_operands: Vec<OperatorTree> = Vec::with_capacity(operands.len());
                let mut any_pushed = false;
                for child in operands {
                    if Self::filter_applies_to(&field, &child) {
                        new_operands.push(OperatorTree::Filter {
                            field: field.clone(),
                            predicate: predicate.clone(),
                            source: Some(Box::new(child)),
                        });
                        any_pushed = true;
                    } else {
                        new_operands.push(child);
                    }
                }
                if any_pushed {
                    let recursed: Vec<OperatorTree> = new_operands
                        .into_iter()
                        .map(|o| self.push_filters_down(o))
                        .collect();
                    return self.recurse_children(OperatorTree::Intersect(recursed));
                }
                // No push happened; rebuild the original Filter.
                return OperatorTree::Filter {
                    field,
                    predicate,
                    source: Some(Box::new(
                        self.recurse_children(OperatorTree::Intersect(new_operands)),
                    )),
                };
            }
            // Source is something else; just recurse through it.
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.recurse_children(*s))),
            };
        }
        self.recurse_children(op)
    }

    // ---------------------------------------------------------------
    // 8. Reorder Intersect by estimated cardinality
    // ---------------------------------------------------------------

    pub(super) fn reorder_intersect(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(operands) = op {
            let mut children: Vec<OperatorTree> = operands
                .into_iter()
                .map(|c| self.recurse_children(c))
                .collect();
            // Match UQA behavior for: the optimizer ranks intersect arms by the
            // algebraic operator cost (`CostModel.estimate`), not the
            // cardinality estimator. The two diverge for `Filter`,
            // `Score`, `Traverse`, `RegularPathQuery`, fusion / hybrid
            // / cross-paradigm join nodes, and any operator with a
            // dedicated formula in `cost_model`.
            let cost_stats = uqa_core::IndexStats::new(self.row_count.unwrap_or(1_000));
            children.sort_by(|a, b| {
                let ca = self.cost_model.estimate(a, &cost_stats);
                let cb = self.cost_model.estimate(b, &cost_stats);
                ca.total_cmp(&cb)
            });
            return OperatorTree::Intersect(children);
        }
        self.recurse_children(op)
    }

    // ---------------------------------------------------------------
    // 9. Reorder fusion signals
    // ---------------------------------------------------------------

    pub(super) fn reorder_fusion_signals(&self, op: OperatorTree) -> OperatorTree {
        match op {
            OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
                let mut signals: Vec<_> = signals
                    .into_iter()
                    .map(|signal| self.reorder_fusion_signals(signal))
                    .collect();
                signals.sort_by(|left, right| {
                    self.graph_aware_signal_cost(left)
                        .total_cmp(&self.graph_aware_signal_cost(right))
                });
                OperatorTree::BayesianEvidenceFusion { signals, base_rate }
            }
            OperatorTree::RobustPositiveEvidencePool {
                signals,
                alpha,
                gating,
                weights,
                logit_min,
                logit_max,
                adaptive_weights,
            } => {
                let mut indexed_signals: Vec<(usize, OperatorTree)> = signals
                    .into_iter()
                    .enumerate()
                    .map(|(index, signal)| (index, self.reorder_fusion_signals(signal)))
                    .collect();
                indexed_signals.sort_by(|(_, left), (_, right)| {
                    let ca = self.graph_aware_signal_cost(left);
                    let cb = self.graph_aware_signal_cost(right);
                    ca.total_cmp(&cb)
                });
                let order: Vec<usize> = indexed_signals
                    .iter()
                    .map(|(original_index, _)| *original_index)
                    .collect();
                let reordered_weights =
                    weights.map(|values| order.iter().map(|index| values[*index]).collect());
                let reordered_logit_min =
                    logit_min.map(|values| order.iter().map(|index| values[*index]).collect());
                let reordered_logit_max =
                    logit_max.map(|values| order.iter().map(|index| values[*index]).collect());
                OperatorTree::RobustPositiveEvidencePool {
                    signals: indexed_signals
                        .into_iter()
                        .map(|(_, signal)| signal)
                        .collect(),
                    alpha,
                    gating,
                    weights: reordered_weights,
                    logit_min: reordered_logit_min,
                    logit_max: reordered_logit_max,
                    adaptive_weights,
                }
            }
            OperatorTree::ProbBoolFusion { signals, mode } => {
                let mut sigs: Vec<OperatorTree> = signals
                    .into_iter()
                    .map(|s| self.reorder_fusion_signals(s))
                    .collect();
                sigs.sort_by(|a, b| {
                    let ca = self.graph_aware_signal_cost(a);
                    let cb = self.graph_aware_signal_cost(b);
                    ca.total_cmp(&cb)
                });
                OperatorTree::ProbBoolFusion {
                    signals: sigs,
                    mode,
                }
            }
            other => self.recurse_fusion(other),
        }
    }

    pub(super) fn graph_aware_signal_cost(&self, signal: &OperatorTree) -> f64 {
        let base = self.estimator.estimate_operator(signal, self.row_count);
        if self.graph_stats.is_some()
            && matches!(
                signal,
                OperatorTree::Traverse { .. }
                    | OperatorTree::PatternMatch { .. }
                    | OperatorTree::RegularPathQuery { .. }
            )
        {
            base * 0.5
        } else {
            base
        }
    }

    pub(super) fn recurse_fusion(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.reorder_fusion_signals(child))
    }

    pub(super) fn filter_applies_to(field: &str, target: &OperatorTree) -> bool {
        match target {
            OperatorTree::Term {
                field: term_field, ..
            } => match term_field {
                Some(f) => f == field,
                None => true,
            },
            OperatorTree::Filter {
                field: filter_field,
                ..
            } => filter_field == field,
            OperatorTree::Intersect(ops) => ops.iter().any(|c| Self::filter_applies_to(field, c)),
            _ => false,
        }
    }
}
