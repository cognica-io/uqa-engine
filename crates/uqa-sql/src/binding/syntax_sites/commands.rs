//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar positions in a stored mutation retain the original command clause order.

use super::*;

/// The scalar expressions a data-modifying `WITH` query owns in syntax order; planner-added view checks and target predicates have no syntax.
pub(super) fn syntax_expressions(command: &CommandPlan) -> Vec<&ScalarExpr> {
    use crate::plan::{ConflictActionPlan, MergeWhenPlan};
    let mut expressions = Vec::new();
    match command {
        CommandPlan::Insert(plan) => {
            expressions.extend(
                plan.columns
                    .iter()
                    .flat_map(crate::ast::AssignmentTarget::expressions),
            );
            expressions.extend(plan.rows.iter().flatten());
            if let Some(conflict) = &plan.on_conflict {
                expressions.extend(&conflict.expressions);
                expressions.extend(conflict.predicate.as_deref());
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &conflict.action
                {
                    expressions.extend(
                        assignments
                            .iter()
                            .flat_map(crate::plan::AssignmentPlan::expressions),
                    );
                    expressions.extend(predicate.as_deref());
                }
            }
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Update(plan) => {
            expressions.extend(
                plan.assignments
                    .iter()
                    .flat_map(crate::plan::AssignmentPlan::expressions),
            );
            expressions.extend(plan.predicate.as_ref());
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Delete(plan) => {
            expressions.extend(plan.predicate.as_ref());
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Merge(plan) => {
            expressions.push(&plan.join_condition);
            for clause in &plan.when_clauses {
                match clause {
                    MergeWhenPlan::UpdateMatched {
                        condition,
                        assignments,
                    }
                    | MergeWhenPlan::UpdateNotMatchedBySource {
                        condition,
                        assignments,
                    } => {
                        expressions.extend(condition.as_ref());
                        expressions.extend(
                            assignments
                                .iter()
                                .flat_map(crate::plan::AssignmentPlan::expressions),
                        );
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition,
                        columns,
                        values,
                        ..
                    } => {
                        expressions.extend(condition.as_ref());
                        expressions.extend(
                            columns
                                .iter()
                                .flat_map(crate::ast::AssignmentTarget::expressions),
                        );
                        expressions.extend(values.iter());
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        expressions.extend(condition.as_ref());
                    }
                }
            }
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        _ => {}
    }
    expressions
}
