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

impl CommandPlan {
    /// Scalar roots owned by this mutation, excluding its relational children.
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        let (insert, update, delete, merge) = match self {
            Self::Insert(plan) => (Some(&**plan), None, None, None),
            Self::Update(plan) => (None, Some(&**plan), None, None),
            Self::Delete(plan) => (None, None, Some(&**plan), None),
            Self::Merge(plan) => (None, None, None, Some(&**plan)),
            _ => (None, None, None, None),
        };
        insert
            .into_iter()
            .flat_map(InsertPlan::expressions)
            .chain(update.into_iter().flat_map(UpdatePlan::expressions))
            .chain(delete.into_iter().flat_map(DeletePlan::expressions))
            .chain(merge.into_iter().flat_map(MergePlan::expressions))
    }

    /// Mutable scalar roots in the same ownership order as `expressions`.
    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        let (insert, update, delete, merge) = match self {
            Self::Insert(plan) => (Some(&mut **plan), None, None, None),
            Self::Update(plan) => (None, Some(&mut **plan), None, None),
            Self::Delete(plan) => (None, None, Some(&mut **plan), None),
            Self::Merge(plan) => (None, None, None, Some(&mut **plan)),
            _ => (None, None, None, None),
        };
        insert
            .into_iter()
            .flat_map(InsertPlan::expressions_mut)
            .chain(update.into_iter().flat_map(UpdatePlan::expressions_mut))
            .chain(delete.into_iter().flat_map(DeletePlan::expressions_mut))
            .chain(merge.into_iter().flat_map(MergePlan::expressions_mut))
    }
}

impl InsertPlan {
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.columns
            .iter()
            .flat_map(AssignmentTarget::expressions)
            .chain(self.rows.iter().flatten())
            .chain(self.on_conflict.iter().flat_map(ConflictPlan::expressions))
            .chain(self.returning.iter().map(|projection| &projection.expr))
            .chain(self.view_checks.iter().map(|check| &check.predicate))
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
            .flat_map(|conflict| conflict.action.expressions())
    }
}

impl ConflictPlan {
    fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.expressions
            .iter()
            .chain(self.predicate.as_deref())
            .chain(self.action.expressions())
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.expressions
            .iter_mut()
            .chain(self.predicate.as_deref_mut())
            .chain(self.action.expressions_mut())
    }
}

impl ConflictActionPlan {
    fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        let (assignments, predicate): (&[AssignmentPlan], _) = match self {
            Self::Nothing => (&[], None),
            Self::Update {
                assignments,
                predicate,
            } => (assignments, predicate.as_deref()),
        };
        assignments
            .iter()
            .flat_map(AssignmentPlan::expressions)
            .chain(predicate)
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
        self.assignments
            .iter()
            .flat_map(AssignmentPlan::expressions)
            .chain(self.predicate.as_ref())
            .chain(self.returning.iter().map(|projection| &projection.expr))
            .chain(self.view_checks.iter().map(|check| &check.predicate))
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
        self.predicate
            .iter()
            .chain(self.returning.iter().map(|projection| &projection.expr))
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
        self.target_predicate
            .iter()
            .chain(std::iter::once(&self.join_condition))
            .chain(
                self.when_clauses
                    .iter()
                    .flat_map(MergeWhenPlan::expressions),
            )
            .chain(self.returning.iter().map(|projection| &projection.expr))
            .chain(self.view_checks.iter().map(|check| &check.predicate))
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
    fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
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
            .chain(assignments.iter().flat_map(AssignmentPlan::expressions))
            .chain(columns.iter().flat_map(AssignmentTarget::expressions))
            .chain(values)
    }

    fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
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
