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
        self.assignment_target_named(target, &target.column)
    }

    pub(super) fn assignment_target_named(
        &mut self,
        target: &AssignmentTarget<ScalarExpr>,
        name: &str,
    ) -> Result<AssignmentTarget<ScalarExpr>> {
        Ok(AssignmentTarget {
            column: self.text(name)?,
            indirection: self.field("indirection", |copying| {
                copying.map(target.indirection.iter(), |copying, step| {
                    Ok(match step {
                        AssignmentStep::Field(field) => AssignmentStep::Field(copying.text(field)?),
                        AssignmentStep::Index(index) => AssignmentStep::Index(
                            copying.field("index", |copying| copying.scalar_box(index))?,
                        ),
                        AssignmentStep::Slice { lower, upper } => AssignmentStep::Slice {
                            lower: copying.field("lower", |copying| {
                                copying.optional_scalar_box(lower.as_deref())
                            })?,
                            upper: copying.field("upper", |copying| {
                                copying.optional_scalar_box(upper.as_deref())
                            })?,
                        },
                    })
                })
            })?,
        })
    }

    pub(super) fn assignment(&mut self, assignment: &AssignmentPlan) -> Result<AssignmentPlan> {
        Ok(AssignmentPlan {
            target: self.field("target", |copying| {
                Ok(match &assignment.target {
                    AssignmentTargets::Single(target) => {
                        AssignmentTargets::Single(copying.assignment_target(target)?)
                    }
                    AssignmentTargets::Multiple(group) => {
                        AssignmentTargets::Multiple(MultipleAssignmentTargets {
                            targets: copying.field("targets", |copying| {
                                copying.map(group.targets.iter(), Self::assignment_target)
                            })?,
                            source_positions: copying
                                .map(group.source_positions.iter(), |_, position| Ok(*position))?,
                            source_width: group.source_width,
                        })
                    }
                })
            })?,
            value: self.field("value", |copying| copying.scalar(&assignment.value))?,
        })
    }

    pub(super) fn conflict(&mut self, conflict: &ConflictPlan) -> Result<ConflictPlan> {
        Ok(ConflictPlan {
            predicate: self.field("predicate", |copying| {
                copying.optional_scalar_box(conflict.predicate.as_deref())
            })?,
            constraint: self.optional_text(conflict.constraint.as_deref())?,
            conflict_columns: self.texts(&conflict.conflict_columns)?,
            expressions: self.field("expressions", |copying| {
                copying.map(conflict.expressions.iter(), Self::scalar)
            })?,
            action: self.field("action", |copying| {
                Ok(match &conflict.action {
                    ConflictActionPlan::Nothing => ConflictActionPlan::Nothing,
                    ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } => ConflictActionPlan::Update {
                        assignments: copying.field("assignments", |copying| {
                            copying.map(assignments.iter(), Self::assignment)
                        })?,
                        predicate: copying.field("predicate", |copying| {
                            copying.optional_scalar_box(predicate.as_deref())
                        })?,
                    },
                })
            })?,
        })
    }

    pub(super) fn merge_when(&mut self, when: &MergeWhenPlan) -> Result<MergeWhenPlan> {
        Ok(match when {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateMatched {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
                assignments: self.field("assignments", |copying| {
                    copying.map(assignments.iter(), Self::assignment)
                })?,
            },
            MergeWhenPlan::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
            },
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateNotMatchedBySource {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
                assignments: self.field("assignments", |copying| {
                    copying.map(assignments.iter(), Self::assignment)
                })?,
            },
            MergeWhenPlan::DeleteNotMatchedBySource { condition } => {
                MergeWhenPlan::DeleteNotMatchedBySource {
                    condition: self.field("condition", |copying| {
                        copying.optional_scalar(condition.as_ref())
                    })?,
                }
            }
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                overriding,
                values,
            } => MergeWhenPlan::InsertNotMatched {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
                columns: self.field("columns", |copying| {
                    copying.map(columns.iter(), Self::assignment_target)
                })?,
                overriding: *overriding,
                values: self.field("values", |copying| copying.map(values.iter(), Self::scalar))?,
            },
            MergeWhenPlan::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
            },
            MergeWhenPlan::NothingNotMatched { condition } => MergeWhenPlan::NothingNotMatched {
                condition: self.field("condition", |copying| {
                    copying.optional_scalar(condition.as_ref())
                })?,
            },
            MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                MergeWhenPlan::NothingNotMatchedBySource {
                    condition: self.field("condition", |copying| {
                        copying.optional_scalar(condition.as_ref())
                    })?,
                }
            }
        })
    }
}
