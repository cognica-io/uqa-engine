//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct relational children of data-modifying commands.

use super::{CommandPlan, CtePlan, ProjectionPlan, QueryPlan, SourcePlan};

impl CommandPlan {
    /// WITH definitions owned by this command, in declaration order.
    pub fn ctes(&self) -> &[CtePlan] {
        match self {
            Self::Insert(plan) => &plan.ctes,
            Self::Update(plan) => &plan.ctes,
            Self::Delete(plan) => &plan.ctes,
            Self::Merge(plan) => &plan.ctes,
            _ => &[],
        }
    }

    pub fn ctes_mut(&mut self) -> Option<&mut Vec<CtePlan>> {
        match self {
            Self::Insert(plan) => Some(&mut plan.ctes),
            Self::Update(plan) => Some(&mut plan.ctes),
            Self::Delete(plan) => Some(&mut plan.ctes),
            Self::Merge(plan) => Some(&mut plan.ctes),
            _ => None,
        }
    }

    /// Query children evaluated in the command's WITH scope. Source-plan subqueries are owned by `source_input`.
    pub fn query_inputs(&self) -> Vec<&QueryPlan> {
        match self {
            Self::Insert(plan) => plan
                .source
                .iter()
                .map(Box::as_ref)
                .chain(plan.subqueries.iter())
                .collect(),
            Self::Update(plan) => plan.subqueries.iter().collect(),
            Self::Delete(plan) => plan.subqueries.iter().collect(),
            Self::Merge(plan) => plan.subqueries.iter().collect(),
            _ => Vec::new(),
        }
    }

    pub fn query_inputs_mut(&mut self) -> Vec<&mut QueryPlan> {
        match self {
            Self::Insert(plan) => plan
                .source
                .iter_mut()
                .map(Box::as_mut)
                .chain(plan.subqueries.iter_mut())
                .collect(),
            Self::Update(plan) => plan.subqueries.iter_mut().collect(),
            Self::Delete(plan) => plan.subqueries.iter_mut().collect(),
            Self::Merge(plan) => plan.subqueries.iter_mut().collect(),
            _ => Vec::new(),
        }
    }

    pub fn source_input(&self) -> Option<&SourcePlan> {
        match self {
            Self::Update(plan) => plan.source.as_deref(),
            Self::Delete(plan) => plan.source.as_deref(),
            Self::Merge(plan) => Some(&plan.source),
            _ => None,
        }
    }

    pub fn source_input_mut(&mut self) -> Option<&mut SourcePlan> {
        match self {
            Self::Update(plan) => plan.source.as_deref_mut(),
            Self::Delete(plan) => plan.source.as_deref_mut(),
            Self::Merge(plan) => Some(&mut plan.source),
            _ => None,
        }
    }

    pub fn mutation_target(&self) -> Option<&str> {
        match self {
            Self::Insert(plan) => Some(&plan.table),
            Self::Update(plan) => Some(&plan.table),
            Self::Delete(plan) => Some(&plan.table),
            Self::Merge(plan) => Some(&plan.target),
            _ => None,
        }
    }

    pub fn mutation_target_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Insert(plan) => Some(&mut plan.table),
            Self::Update(plan) => Some(&mut plan.table),
            Self::Delete(plan) => Some(&mut plan.table),
            Self::Merge(plan) => Some(&mut plan.target),
            _ => None,
        }
    }

    pub fn returning(&self) -> Option<&[ProjectionPlan]> {
        match self {
            Self::Insert(plan) => Some(&plan.returning),
            Self::Update(plan) => Some(&plan.returning),
            Self::Delete(plan) => Some(&plan.returning),
            Self::Merge(plan) => Some(&plan.returning),
            _ => None,
        }
    }
}

impl CommandPlan {
    /// Scalar expressions owned by the command, excluding its relational children.
    pub fn expressions(&self) -> Vec<&super::ScalarExpr> {
        use super::{ConflictActionPlan, MergeWhenPlan};
        let mut expressions = Vec::new();
        match self {
            Self::Insert(plan) => {
                expressions.extend(plan.rows.iter().flatten());
                if let Some(conflict) = &plan.on_conflict {
                    expressions.extend(&conflict.expressions);
                    expressions.extend(conflict.predicate.as_deref());
                    if let ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } = &conflict.action
                    {
                        expressions.extend(assignments.iter().map(|assignment| &assignment.value));
                        expressions.extend(predicate.as_deref());
                    }
                }
                expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
                expressions.extend(plan.view_checks.iter().map(|check| &check.predicate));
            }
            Self::Update(plan) => {
                expressions.extend(plan.assignments.iter().map(|assignment| &assignment.value));
                expressions.extend(plan.predicate.as_ref());
                expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
                expressions.extend(plan.view_checks.iter().map(|check| &check.predicate));
            }
            Self::Delete(plan) => {
                expressions.extend(plan.predicate.as_ref());
                expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
            }
            Self::Merge(plan) => {
                expressions.extend(plan.target_predicate.as_ref());
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
                            expressions
                                .extend(assignments.iter().map(|assignment| &assignment.value));
                        }
                        MergeWhenPlan::InsertNotMatched {
                            condition, values, ..
                        } => {
                            expressions.extend(condition.as_ref());
                            expressions.extend(values);
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
                expressions.extend(plan.view_checks.iter().map(|check| &check.predicate));
            }
            _ => {}
        }
        expressions
    }

    /// Scalar expressions owned by the command, excluding its relational children.
    #[expect(
        clippy::too_many_lines,
        reason = "enumerates scalar ownership for every mutation command"
    )]
    pub fn expressions_mut(&mut self) -> Vec<&mut super::ScalarExpr> {
        use super::{ConflictActionPlan, MergeWhenPlan};
        let mut expressions = Vec::new();
        match self {
            Self::Insert(plan) => {
                expressions.extend(plan.rows.iter_mut().flatten());
                if let Some(conflict) = &mut plan.on_conflict {
                    expressions.extend(&mut conflict.expressions);
                    expressions.extend(conflict.predicate.as_deref_mut());
                    if let ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } = &mut conflict.action
                    {
                        expressions.extend(
                            assignments
                                .iter_mut()
                                .map(|assignment| &mut assignment.value),
                        );
                        expressions.extend(predicate.as_deref_mut());
                    }
                }
                expressions.extend(
                    plan.returning
                        .iter_mut()
                        .map(|projection| &mut projection.expr),
                );
                expressions.extend(
                    plan.view_checks
                        .iter_mut()
                        .map(|check| &mut check.predicate),
                );
            }
            Self::Update(plan) => {
                expressions.extend(
                    plan.assignments
                        .iter_mut()
                        .map(|assignment| &mut assignment.value),
                );
                expressions.extend(plan.predicate.as_mut());
                expressions.extend(
                    plan.returning
                        .iter_mut()
                        .map(|projection| &mut projection.expr),
                );
                expressions.extend(
                    plan.view_checks
                        .iter_mut()
                        .map(|check| &mut check.predicate),
                );
            }
            Self::Delete(plan) => {
                expressions.extend(plan.predicate.as_mut());
                expressions.extend(
                    plan.returning
                        .iter_mut()
                        .map(|projection| &mut projection.expr),
                );
            }
            Self::Merge(plan) => {
                expressions.extend(plan.target_predicate.as_mut());
                expressions.push(&mut plan.join_condition);
                for clause in &mut plan.when_clauses {
                    match clause {
                        MergeWhenPlan::UpdateMatched {
                            condition,
                            assignments,
                        }
                        | MergeWhenPlan::UpdateNotMatchedBySource {
                            condition,
                            assignments,
                        } => {
                            expressions.extend(condition.as_mut());
                            expressions.extend(
                                assignments
                                    .iter_mut()
                                    .map(|assignment| &mut assignment.value),
                            );
                        }
                        MergeWhenPlan::InsertNotMatched {
                            condition, values, ..
                        } => {
                            expressions.extend(condition.as_mut());
                            expressions.extend(values);
                        }
                        MergeWhenPlan::DeleteMatched { condition }
                        | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                        | MergeWhenPlan::NothingMatched { condition }
                        | MergeWhenPlan::NothingNotMatched { condition }
                        | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                            expressions.extend(condition.as_mut());
                        }
                    }
                }
                expressions.extend(
                    plan.returning
                        .iter_mut()
                        .map(|projection| &mut projection.expr),
                );
                expressions.extend(
                    plan.view_checks
                        .iter_mut()
                        .map(|check| &mut check.predicate),
                );
            }
            _ => {}
        }
        expressions
    }

    pub fn scalar_subqueries(&self) -> &[QueryPlan] {
        match self {
            Self::Insert(plan) => &plan.subqueries,
            Self::Update(plan) => &plan.subqueries,
            Self::Delete(plan) => &plan.subqueries,
            Self::Merge(plan) => &plan.subqueries,
            _ => &[],
        }
    }

    pub fn target_qualifier(&self) -> Option<&str> {
        match self {
            Self::Insert(plan) => Some(&plan.target_qualifier),
            Self::Update(plan) => Some(&plan.target_qualifier),
            Self::Delete(plan) => Some(&plan.target_qualifier),
            Self::Merge(plan) => Some(&plan.target_qualifier),
            _ => None,
        }
    }

    pub fn returning_aliases(&self) -> Option<&uqa_sql::ast::ReturningAliases> {
        match self {
            Self::Insert(plan) => Some(&plan.returning_aliases),
            Self::Update(plan) => Some(&plan.returning_aliases),
            Self::Delete(plan) => Some(&plan.returning_aliases),
            Self::Merge(plan) => Some(&plan.returning_aliases),
            _ => None,
        }
    }
}
