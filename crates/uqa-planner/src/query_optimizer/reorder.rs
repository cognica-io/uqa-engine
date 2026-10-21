//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Filter placement and cost ordering of membership-only intersections.

use uqa_operators::OperatorTree;

use super::QueryOptimizer;

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
    // Reorder membership-only intersections by estimated cost.
    // ---------------------------------------------------------------

    pub(super) fn reorder_intersect(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(operands) = op {
            let mut children: Vec<OperatorTree> = operands
                .into_iter()
                .map(|c| self.recurse_children(c))
                .collect();
            // Decorated postings retain their operand order: changing floating-point
            // accumulation or right-biased field precedence can change the complete result.
            if children.iter().all(OperatorTree::is_membership_only) {
                children.sort_by(|a, b| {
                    let ca = self.cost_model.estimate(a, &self.index_stats);
                    let cb = self.cost_model.estimate(b, &self.index_stats);
                    ca.total_cmp(&cb)
                });
            }
            return OperatorTree::Intersect(children);
        }
        self.recurse_children(op)
    }

    pub(super) fn filter_applies_to(field: &str, target: &OperatorTree) -> bool {
        match target {
            OperatorTree::Term {
                field: term_field, ..
            }
            | OperatorTree::Phrase {
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
