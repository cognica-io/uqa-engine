//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result};
use crate::ast::{AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets};
use crate::plan::{AssignmentPlan, ConflictActionPlan, ConflictPlan, MergeWhenPlan};
use crate::{
    ast::{Expr, MergeWhen, OnConflict, OnConflictAction},
    plan::AggregateClassifier,
    ScalarExpr,
};

impl Copying<'_> {
    pub(super) fn lower_assignment_target(
        &mut self,
        target: &AssignmentTarget,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<AssignmentTarget<ScalarExpr>> {
        Ok(AssignmentTarget {
            column: self.text(&target.column)?,
            indirection: self.field("indirection", |copying| {
                copying.map(target.indirection.iter(), |copying, step| {
                    Ok(match step {
                        AssignmentStep::Field(field) => AssignmentStep::Field(copying.text(field)?),
                        AssignmentStep::Index(index) => {
                            AssignmentStep::Index(copying.field("index", |copying| {
                                copying.lower_scalar_box(index, aggregates, arena)
                            })?)
                        }
                        AssignmentStep::Slice { lower, upper } => AssignmentStep::Slice {
                            lower: copying.field("lower", |copying| {
                                copying.lower_optional_scalar_box(
                                    lower.as_deref(),
                                    aggregates,
                                    arena,
                                )
                            })?,
                            upper: copying.field("upper", |copying| {
                                copying.lower_optional_scalar_box(
                                    upper.as_deref(),
                                    aggregates,
                                    arena,
                                )
                            })?,
                        },
                    })
                })
            })?,
        })
    }

    pub(super) fn lower_assignment(
        &mut self,
        assignment: &(AssignmentTargets, Expr),
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<AssignmentPlan> {
        Ok(AssignmentPlan {
            target: self.field("target", |copying| {
                Ok(match &assignment.0 {
                    AssignmentTargets::Single(target) => AssignmentTargets::Single(
                        copying.lower_assignment_target(target, aggregates, arena)?,
                    ),
                    AssignmentTargets::Multiple(group) => {
                        AssignmentTargets::Multiple(MultipleAssignmentTargets {
                            targets: copying.field("targets", |copying| {
                                copying.map(group.targets.iter(), |copying, target| {
                                    copying.lower_assignment_target(target, aggregates, arena)
                                })
                            })?,
                            source_positions: copying
                                .map(group.source_positions.iter(), |_, position| Ok(*position))?,
                            source_width: group.source_width,
                        })
                    }
                })
            })?,
            value: self.field("value", |copying| {
                copying.lower_scalar(&assignment.1, aggregates, arena)
            })?,
        })
    }

    pub(super) fn lower_conflict(
        &mut self,
        conflict: &OnConflict,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ConflictPlan> {
        let action = self.field("action", |copying| {
            Ok(match &conflict.action {
                OnConflictAction::Nothing => ConflictActionPlan::Nothing,
                OnConflictAction::Update {
                    assignments,
                    r#where,
                } => ConflictActionPlan::Update {
                    assignments: copying.field("assignments", |copying| {
                        copying.map(assignments.iter(), |copying, assignment| {
                            copying.lower_assignment(assignment, aggregates, arena)
                        })
                    })?,
                    predicate: copying.field("predicate", |copying| {
                        copying.lower_optional_scalar_box(r#where.as_deref(), aggregates, arena)
                    })?,
                },
            })
        })?;
        Ok(ConflictPlan {
            predicate: self.field("predicate", |copying| {
                copying.lower_optional_scalar_box(conflict.predicate.as_deref(), aggregates, arena)
            })?,
            constraint: self.optional_text(conflict.constraint.as_deref())?,
            conflict_columns: self.texts(&conflict.conflict_columns)?,
            expressions: self.field("expressions", |copying| {
                copying.map(conflict.expressions.iter(), |copying, expression| {
                    copying.lower_scalar(expression, aggregates, arena)
                })
            })?,
            action,
        })
    }

    pub(super) fn lower_merge_when(
        &mut self,
        when: &MergeWhen,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<MergeWhenPlan> {
        Ok(match when {
            MergeWhen::UpdateMatched {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateMatched {
                condition: self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?,
                assignments: self.field("assignments", |copying| {
                    copying.map(assignments.iter(), |copying, assignment| {
                        copying.lower_assignment(assignment, aggregates, arena)
                    })
                })?,
            },
            MergeWhen::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
                condition: self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?,
            },
            MergeWhen::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => MergeWhenPlan::UpdateNotMatchedBySource {
                condition: self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?,
                assignments: self.field("assignments", |copying| {
                    copying.map(assignments.iter(), |copying, assignment| {
                        copying.lower_assignment(assignment, aggregates, arena)
                    })
                })?,
            },
            MergeWhen::DeleteNotMatchedBySource { condition } => {
                MergeWhenPlan::DeleteNotMatchedBySource {
                    condition: self.field("condition", |copying| {
                        copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                    })?,
                }
            }
            MergeWhen::InsertNotMatched {
                condition,
                columns,
                overriding,
                values,
            } => {
                let condition = self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?;
                let values = self.field("values", |copying| {
                    copying.map(values.iter(), |copying, expression| {
                        copying.lower_scalar(expression, aggregates, arena)
                    })
                })?;
                MergeWhenPlan::InsertNotMatched {
                    condition,
                    columns: self.field("columns", |copying| {
                        copying.map(columns.iter(), |copying, target| {
                            copying.lower_assignment_target(target, aggregates, arena)
                        })
                    })?,
                    overriding: *overriding,
                    values,
                }
            }
            MergeWhen::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
                condition: self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?,
            },
            MergeWhen::NothingNotMatched { condition } => MergeWhenPlan::NothingNotMatched {
                condition: self.field("condition", |copying| {
                    copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                })?,
            },
            MergeWhen::NothingNotMatchedBySource { condition } => {
                MergeWhenPlan::NothingNotMatchedBySource {
                    condition: self.field("condition", |copying| {
                        copying.lower_optional_scalar(condition.as_ref(), aggregates, arena)
                    })?,
                }
            }
        })
    }
}
