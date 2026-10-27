//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::ast::{AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets};
use crate::plan::{AssignmentPlan, ConflictActionPlan, ConflictPlan, MergeWhenPlan};
use crate::ScalarExpr;

impl Copying<'_> {
    pub(super) fn assignment_target(
        &mut self,
        target: &AssignmentTarget<ScalarExpr>,
    ) -> Result<AssignmentTarget<ScalarExpr>> {
        Ok(AssignmentTarget {
            column: self.text(&target.column)?,
            indirection: self.map(target.indirection.iter(), |copying, step| {
                Ok(match step {
                    AssignmentStep::Field(field) => AssignmentStep::Field(copying.text(field)?),
                    AssignmentStep::Index(index) => {
                        AssignmentStep::Index(copying.scalar_box(index)?)
                    }
                    AssignmentStep::Slice { lower, upper } => AssignmentStep::Slice {
                        lower: copying.optional_scalar_box(lower.as_deref())?,
                        upper: copying.optional_scalar_box(upper.as_deref())?,
                    },
                })
            })?,
        })
    }

    pub(super) fn assignment(&mut self, assignment: &AssignmentPlan) -> Result<AssignmentPlan> {
        Ok(AssignmentPlan {
            target: match &assignment.target {
                AssignmentTargets::Single(target) => {
                    AssignmentTargets::Single(self.assignment_target(target)?)
                }
                AssignmentTargets::Multiple(group) => {
                    AssignmentTargets::Multiple(MultipleAssignmentTargets {
                        targets: self.map(group.targets.iter(), Self::assignment_target)?,
                        source_positions: self
                            .map(group.source_positions.iter(), |_, position| Ok(*position))?,
                        source_width: group.source_width,
                    })
                }
            },
            value: self.scalar(&assignment.value)?,
        })
    }

    pub(super) fn conflict(&mut self, conflict: &ConflictPlan) -> Result<ConflictPlan> {
        Ok(ConflictPlan {
            predicate: self.optional_scalar_box(conflict.predicate.as_deref())?,
            constraint: self.optional_text(conflict.constraint.as_deref())?,
            conflict_columns: self.texts(&conflict.conflict_columns)?,
            expressions: self.map(conflict.expressions.iter(), Self::scalar)?,
            action: match &conflict.action {
                ConflictActionPlan::Nothing => ConflictActionPlan::Nothing,
                ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } => ConflictActionPlan::Update {
                    assignments: self.map(assignments.iter(), Self::assignment)?,
                    predicate: self.optional_scalar_box(predicate.as_deref())?,
                },
            },
        })
    }

    pub(super) fn merge_when(&mut self, when: &MergeWhenPlan) -> Result<MergeWhenPlan> {
        Ok(match when {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateMatched {
                condition: self.optional_scalar(condition.as_ref())?,
                assignments: self.map(assignments.iter(), Self::assignment)?,
            },
            MergeWhenPlan::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
                condition: self.optional_scalar(condition.as_ref())?,
            },
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateNotMatchedBySource {
                condition: self.optional_scalar(condition.as_ref())?,
                assignments: self.map(assignments.iter(), Self::assignment)?,
            },
            MergeWhenPlan::DeleteNotMatchedBySource { condition } => {
                MergeWhenPlan::DeleteNotMatchedBySource {
                    condition: self.optional_scalar(condition.as_ref())?,
                }
            }
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                overriding,
                values,
            } => MergeWhenPlan::InsertNotMatched {
                condition: self.optional_scalar(condition.as_ref())?,
                columns: self.map(columns.iter(), Self::assignment_target)?,
                overriding: *overriding,
                values: self.map(values.iter(), Self::scalar)?,
            },
            MergeWhenPlan::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
                condition: self.optional_scalar(condition.as_ref())?,
            },
            MergeWhenPlan::NothingNotMatched { condition } => MergeWhenPlan::NothingNotMatched {
                condition: self.optional_scalar(condition.as_ref())?,
            },
            MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                MergeWhenPlan::NothingNotMatchedBySource {
                    condition: self.optional_scalar(condition.as_ref())?,
                }
            }
        })
    }
}
