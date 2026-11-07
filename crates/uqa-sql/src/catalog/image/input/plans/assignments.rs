//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{invalid, retirement::Pending, Input, PlanMemory, SQLError, ScalarExpr};
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets},
    plan::{AssignmentPlan, ConflictActionPlan, ConflictPlan, MergeWhenPlan, ViewCheckPlan},
};

impl Input<'_> {
    pub(super) fn plan_targets(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<AssignmentTarget<ScalarExpr>>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            input.plan_target(memory, references)
        })
    }

    fn plan_target(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<AssignmentTarget<ScalarExpr>>, SQLError> {
        let column = self.plan_text(memory)?;
        let indirection = self.plan_scalar_vector(memory, |input, memory| {
            input.plan_assignment_step(memory, references)
        })?;
        Ok(Pending::new(AssignmentTarget {
            column,
            indirection: indirection.take(),
        }))
    }

    fn plan_assignment_step(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<AssignmentStep<ScalarExpr>>, SQLError> {
        Ok(Pending::new(match self.binary.array::<1>()?[0] {
            0 => AssignmentStep::Field(self.plan_text(memory)?),
            1 => {
                memory.grow(size_of::<ScalarExpr>())?;
                AssignmentStep::Index(Box::new(self.plan_scalar(memory, references)?.take()))
            }
            2 => {
                let lower = self.plan_optional_scalar_box(memory, references)?;
                let upper = self.plan_optional_scalar_box(memory, references)?;
                AssignmentStep::Slice {
                    lower: lower.take(),
                    upper: upper.take(),
                }
            }
            _ => return Err(invalid("query assignment step")),
        }))
    }

    fn plan_assignment_targets(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<AssignmentTargets<ScalarExpr>>, SQLError> {
        Ok(Pending::new(match self.binary.array::<1>()?[0] {
            0 => AssignmentTargets::Single(self.plan_target(memory, references)?.take()),
            1 => {
                let targets = self.plan_targets(memory, references)?;
                let source_positions =
                    self.plan_vector(memory, |input, _| Ok(input.binary.count()?))?;
                let source_width = self.binary.count()?;
                if targets.len() != source_positions.len() {
                    return Err(invalid("multiple assignment address count"));
                }
                for position in &source_positions {
                    self.binary.cancellation.check()?;
                    if *position >= source_width {
                        return Err(invalid("multiple assignment source address"));
                    }
                }
                AssignmentTargets::Multiple(MultipleAssignmentTargets {
                    targets: targets.take(),
                    source_positions,
                    source_width,
                })
            }
            _ => return Err(invalid("query assignment target")),
        }))
    }

    pub(super) fn plan_assignments(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<AssignmentPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            let target = input.plan_assignment_targets(memory, references)?;
            let value = input.plan_scalar(memory, references)?;
            Ok(Pending::new(AssignmentPlan {
                target: target.take(),
                value: value.take(),
            }))
        })
    }

    pub(super) fn plan_conflict(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Option<ConflictPlan>>, SQLError> {
        if !self.flag()? {
            return Ok(Pending::new(None));
        }
        let predicate = self.plan_optional_scalar_box(memory, references)?;
        let constraint = self.plan_optional_text(memory)?;
        let conflict_columns = self.plan_texts(memory)?;
        let expressions = self.plan_scalars(memory, references)?;
        let action = match self.binary.array::<1>()?[0] {
            0 => ConflictActionPlan::Nothing,
            1 => {
                let assignments = self.plan_assignments(memory, references)?;
                let predicate = self.plan_optional_scalar_box(memory, references)?;
                ConflictActionPlan::Update {
                    assignments: assignments.take(),
                    predicate: predicate.take(),
                }
            }
            _ => return Err(invalid("query conflict action")),
        };
        Ok(Pending::new(Some(ConflictPlan {
            predicate: predicate.take(),
            constraint,
            conflict_columns,
            expressions: expressions.take(),
            action,
        })))
    }

    pub(super) fn plan_view_checks(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<ViewCheckPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            let view = input.plan_text(memory)?;
            let predicate = input.plan_scalar(memory, references)?;
            Ok(Pending::new(ViewCheckPlan {
                view,
                predicate: predicate.take(),
            }))
        })
    }

    pub(super) fn plan_when_clauses(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<MergeWhenPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| input.plan_when(memory, references))
    }

    fn plan_when(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<MergeWhenPlan>, SQLError> {
        let tag = self.binary.array::<1>()?[0];
        if tag > 7 {
            return Err(invalid("query merge action"));
        }
        let condition = self.plan_optional_scalar(memory, references)?;
        Ok(Pending::new(match tag {
            0 | 2 => {
                let assignments = self.plan_assignments(memory, references)?;
                if tag == 0 {
                    MergeWhenPlan::UpdateMatched {
                        condition: condition.take(),
                        assignments: assignments.take(),
                    }
                } else {
                    MergeWhenPlan::UpdateNotMatchedBySource {
                        condition: condition.take(),
                        assignments: assignments.take(),
                    }
                }
            }
            1 => MergeWhenPlan::DeleteMatched {
                condition: condition.take(),
            },
            3 => MergeWhenPlan::DeleteNotMatchedBySource {
                condition: condition.take(),
            },
            4 => {
                let columns = self.plan_targets(memory, references)?;
                let overriding = self.plan_overriding()?;
                let values = self.plan_scalars(memory, references)?;
                MergeWhenPlan::InsertNotMatched {
                    condition: condition.take(),
                    columns: columns.take(),
                    overriding,
                    values: values.take(),
                }
            }
            5 => MergeWhenPlan::NothingMatched {
                condition: condition.take(),
            },
            6 => MergeWhenPlan::NothingNotMatched {
                condition: condition.take(),
            },
            7 => MergeWhenPlan::NothingNotMatchedBySource {
                condition: condition.take(),
            },
            _ => unreachable!("validated merge action"),
        }))
    }
}
