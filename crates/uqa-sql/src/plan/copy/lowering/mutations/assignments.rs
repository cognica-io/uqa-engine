//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation fields share retained construction and keep completed scalar prefixes owned through later callbacks.

use super::super::input::InlineScalarInput;
use super::{Copying, QueryArena, Result};
use crate::{
    ast::{AssignmentTarget, AssignmentTargets, Expr, MergeWhen, OnConflict},
    plan::retirement::Pending,
    plan::{AggregateClassifier, AssignmentPlan, ConflictPlan, MergeWhenPlan},
    ScalarExpr,
};

mod conflict;
mod input;
mod merge;
mod targets;
#[cfg(test)]
mod tests;

impl Copying<'_> {
    pub(super) fn lower_assignment_targets(
        &mut self,
        targets: &[AssignmentTarget],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<AssignmentTarget<ScalarExpr>>> {
        crate::catalog::work::inline(targets::construct_targets(
            targets,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }

    pub(super) fn lower_assignments(
        &mut self,
        assignments: &[(AssignmentTargets, Expr)],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<AssignmentPlan>> {
        crate::catalog::work::inline(input::construct_assignments(
            assignments,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }

    pub(super) fn lower_conflict(
        &mut self,
        conflict: &OnConflict,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ConflictPlan> {
        crate::catalog::work::inline(conflict::construct_conflict(
            conflict,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }

    pub(super) fn lower_merge_whens(
        &mut self,
        clauses: &[MergeWhen],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<MergeWhenPlan>> {
        crate::catalog::work::inline(merge::construct_merge_whens(
            clauses,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }
}
