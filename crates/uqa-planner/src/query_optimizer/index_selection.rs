//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical index-scan selection.

use uqa_operators::OperatorTree;

use crate::OperatorKind;

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // 10. Substitute leaf Filter with IndexScan
    // ---------------------------------------------------------------

    pub(super) fn apply_index_scan(&self, op: OperatorTree) -> OperatorTree {
        let Some(table) = &self.table_name else {
            return self.recurse_index_scan(op);
        };
        if let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = op
        {
            let best = self
                .index_candidates
                .iter()
                .filter(|candidate| {
                    candidate.table_name == *table
                        && candidate.field == field
                        && candidate.predicate == predicate
                        && candidate.scan_cost.is_finite()
                        && candidate.scan_cost >= 0.0
                })
                .min_by(|left, right| left.scan_cost.total_cmp(&right.scan_cost));
            if let Some(candidate) = best {
                // Prefer the index only when its `scan_cost(predicate)` beats
                // a full scan; otherwise keep the filter over the original
                // operator.
                let full_scan_cost = self
                    .cost_model
                    .physical_cost
                    .estimate_unary(OperatorKind::TableScan, self.index_stats.total_docs as f64)
                    .total();
                if candidate.scan_cost < full_scan_cost {
                    return OperatorTree::IndexScan {
                        index_name: candidate.index_name.clone(),
                        field,
                        predicate,
                    };
                }
            }
            return OperatorTree::Filter {
                field,
                predicate,
                source: None,
            };
        }
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.apply_index_scan(*s))),
            };
        }
        self.recurse_index_scan(op)
    }

    pub(super) fn recurse_index_scan(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.apply_index_scan(child))
    }
}
