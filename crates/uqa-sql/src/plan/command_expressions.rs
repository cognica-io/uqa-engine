//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the direct scalar roots of mutations without collecting reference arrays.

use super::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan,
    MergePlan, MergeWhenPlan, ScalarExpr, UpdatePlan,
};
use crate::ast::AssignmentTarget;

enum MutationExpressions<I, U, D, M> {
    Insert(I),
    Update(U),
    Delete(D),
    Merge(M),
    Empty,
}

impl<I, U, D, M> Iterator for MutationExpressions<I, U, D, M>
where
    I: Iterator,
    U: Iterator<Item = I::Item>,
    D: Iterator<Item = I::Item>,
    M: Iterator<Item = I::Item>,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Insert(items) => items.next(),
            Self::Update(items) => items.next(),
            Self::Delete(items) => items.next(),
            Self::Merge(items) => items.next(),
            Self::Empty => None,
        }
    }
}

impl CommandPlan {
    /// Scalar roots owned by this mutation, excluding its relational children.
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    pub(in crate::plan) fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        match self {
            Self::Insert(plan) => MutationExpressions::Insert(plan.expression_steps()),
            Self::Update(plan) => MutationExpressions::Update(plan.expression_steps()),
            Self::Delete(plan) => MutationExpressions::Delete(plan.expression_steps()),
            Self::Merge(plan) => MutationExpressions::Merge(plan.expression_steps()),
            _ => MutationExpressions::Empty,
        }
    }

    /// Mutable scalar roots in the same ownership order as `expressions`.
    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        match self {
            Self::Insert(plan) => MutationExpressions::Insert(plan.expressions_mut()),
            Self::Update(plan) => MutationExpressions::Update(plan.expressions_mut()),
            Self::Delete(plan) => MutationExpressions::Delete(plan.expressions_mut()),
            Self::Merge(plan) => MutationExpressions::Merge(plan.expressions_mut()),
            _ => MutationExpressions::Empty,
        }
    }
}

impl InsertPlan {
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.target_steps()
            .chain(self.row_steps())
            .chain(self.result_steps())
    }

    pub(in crate::plan) fn target_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.columns
            .iter()
            .flat_map(|target| std::iter::once(None).chain(target.expression_steps()))
    }

    pub(in crate::plan) fn row_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.rows
            .iter()
            .flat_map(|row| std::iter::once(None).chain(row.iter().map(Some)))
    }

    pub(in crate::plan) fn result_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.on_conflict
            .iter()
            .flat_map(ConflictPlan::expression_steps)
            .chain(
                self.returning
                    .iter()
                    .map(|projection| Some(&projection.expr)),
            )
            .chain(self.view_checks.iter().map(|check| Some(&check.predicate)))
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.columns
            .iter_mut()
            .flat_map(AssignmentTarget::expressions_mut)
            .chain(self.rows.iter_mut().flatten())
            .chain(
                self.on_conflict
                    .iter_mut()
                    .flat_map(ConflictPlan::expressions_mut),
            )
            .chain(
                self.returning
                    .iter_mut()
                    .map(|projection| &mut projection.expr),
            )
            .chain(
                self.view_checks
                    .iter_mut()
                    .map(|check| &mut check.predicate),
            )
    }

    /// Expressions that see the additional EXCLUDED relation in ON CONFLICT DO UPDATE.
    pub fn conflict_update_expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.on_conflict
            .iter()
            .flat_map(|conflict| conflict.action.expression_steps().flatten())
    }
}

impl ConflictPlan {
    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.expressions
            .iter()
            .map(Some)
            .chain(self.predicate.as_deref().map(Some))
            .chain(self.action.expression_steps())
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.expressions
            .iter_mut()
            .chain(self.predicate.as_deref_mut())
            .chain(self.action.expressions_mut())
    }
}

impl ConflictActionPlan {
    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        let (assignments, predicate): (&[AssignmentPlan], _) = match self {
            Self::Nothing => (&[], None),
            Self::Update {
                assignments,
                predicate,
            } => (assignments, predicate.as_deref()),
        };
        assignments
            .iter()
            .flat_map(AssignmentPlan::expression_steps)
            .chain(predicate.map(Some))
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        let (assignments, predicate): (&mut [AssignmentPlan], _) = match self {
            Self::Nothing => (&mut [], None),
            Self::Update {
                assignments,
                predicate,
            } => (assignments, predicate.as_deref_mut()),
        };
        assignments
            .iter_mut()
            .flat_map(AssignmentPlan::expressions_mut)
            .chain(predicate)
    }
}

impl UpdatePlan {
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.assignments
            .iter()
            .flat_map(AssignmentPlan::expression_steps)
            .chain(self.predicate.as_ref().map(Some))
            .chain(
                self.returning
                    .iter()
                    .map(|projection| Some(&projection.expr)),
            )
            .chain(self.view_checks.iter().map(|check| Some(&check.predicate)))
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.assignments
            .iter_mut()
            .flat_map(AssignmentPlan::expressions_mut)
            .chain(self.predicate.as_mut())
            .chain(
                self.returning
                    .iter_mut()
                    .map(|projection| &mut projection.expr),
            )
            .chain(
                self.view_checks
                    .iter_mut()
                    .map(|check| &mut check.predicate),
            )
    }
}

impl DeletePlan {
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.predicate.iter().map(Some).chain(
            self.returning
                .iter()
                .map(|projection| Some(&projection.expr)),
        )
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.predicate.iter_mut().chain(
            self.returning
                .iter_mut()
                .map(|projection| &mut projection.expr),
        )
    }
}

impl MergePlan {
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        self.target_predicate
            .iter()
            .map(Some)
            .chain(std::iter::once(Some(&self.join_condition)))
            .chain(
                self.when_clauses
                    .iter()
                    .flat_map(|arm| std::iter::once(None).chain(arm.expression_steps())),
            )
            .chain(
                self.returning
                    .iter()
                    .map(|projection| Some(&projection.expr)),
            )
            .chain(self.view_checks.iter().map(|check| Some(&check.predicate)))
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.target_predicate
            .iter_mut()
            .chain(std::iter::once(&mut self.join_condition))
            .chain(
                self.when_clauses
                    .iter_mut()
                    .flat_map(MergeWhenPlan::expressions_mut),
            )
            .chain(
                self.returning
                    .iter_mut()
                    .map(|projection| &mut projection.expr),
            )
            .chain(
                self.view_checks
                    .iter_mut()
                    .map(|check| &mut check.predicate),
            )
    }
}

impl MergeWhenPlan {
    /// Select the candidate row visible to this action's condition and operands.
    pub(crate) fn input_scope<T>(&self, matched: T, target: T, source: T) -> T {
        match self {
            Self::UpdateNotMatchedBySource { .. }
            | Self::DeleteNotMatchedBySource { .. }
            | Self::NothingNotMatchedBySource { .. } => target,
            Self::InsertNotMatched { .. } | Self::NothingNotMatched { .. } => source,
            _ => matched,
        }
    }

    pub(crate) fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expression_steps().flatten()
    }

    fn expression_steps(&self) -> impl Iterator<Item = Option<&ScalarExpr>> {
        let (condition, assignments, columns, values): (
            _,
            &[AssignmentPlan],
            &[AssignmentTarget<ScalarExpr>],
            &[ScalarExpr],
        ) = match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => (condition.as_ref(), assignments, &[], &[]),
            Self::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => (condition.as_ref(), &[], columns, values),
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => (condition.as_ref(), &[], &[], &[]),
        };
        condition
            .into_iter()
            .map(Some)
            .chain(
                assignments
                    .iter()
                    .flat_map(AssignmentPlan::expression_steps),
            )
            .chain(
                columns
                    .iter()
                    .flat_map(|target| std::iter::once(None).chain(target.expression_steps())),
            )
            .chain(values.iter().map(Some))
    }

    pub(crate) fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        let (condition, assignments, columns, values): (
            _,
            &mut [AssignmentPlan],
            &mut [AssignmentTarget<ScalarExpr>],
            &mut [ScalarExpr],
        ) = match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => (condition.as_mut(), assignments, &mut [], &mut []),
            Self::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => (condition.as_mut(), &mut [], columns, values),
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => {
                (condition.as_mut(), &mut [], &mut [], &mut [])
            }
        };
        condition
            .into_iter()
            .chain(
                assignments
                    .iter_mut()
                    .flat_map(AssignmentPlan::expressions_mut),
            )
            .chain(
                columns
                    .iter_mut()
                    .flat_map(AssignmentTarget::expressions_mut),
            )
            .chain(values)
    }
}
