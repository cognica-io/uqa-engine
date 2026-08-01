//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical index-scan selection.

use uqa_operators::OperatorTree;

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
        } = &op
        {
            let managed = self
                .index_manager
                .as_ref()
                .and_then(|manager| manager.find_covering_index_with_cost(table, field, predicate));
            let catalog = self
                .index_candidates
                .iter()
                .filter(|candidate| {
                    candidate.table_name == *table
                        && candidate.field == *field
                        && candidate.predicate == *predicate
                        && candidate.scan_cost.is_finite()
                        && candidate.scan_cost >= 0.0
                })
                .map(|candidate| (candidate.index_name.clone(), candidate.scan_cost))
                .min_by(|left, right| left.1.total_cmp(&right.1));
            let best = match (managed, catalog) {
                (Some(left), Some(right)) if left.1 <= right.1 => Some(left),
                (Some(_), Some(right)) => Some(right),
                (Some(candidate), None) | (None, Some(candidate)) => Some(candidate),
                (None, None) => None,
            };
            if let Some((name, scan_cost)) = best {
                // the canonical UQA implementation's `_apply_index_scan` only rewrites when the
                // index's `scan_cost(predicate)` beats a full scan.
                // Mirror that gate exactly: prefer the index only when
                // its cost is strictly cheaper.
                let full_scan_cost = self.row_count.unwrap_or(0) as f64;
                if scan_cost < full_scan_cost {
                    return OperatorTree::IndexScan {
                        index_name: name,
                        field: field.clone(),
                        predicate: predicate.clone(),
                    };
                }
            }
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
