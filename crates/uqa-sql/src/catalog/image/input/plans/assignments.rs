//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{invalid, retirement::Pending, Fields, Input, PlanMemory, SQLError, ScalarExpr};
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets},
    plan::{AssignmentPlan, ConflictActionPlan, ConflictPlan, MergeWhenPlan, ViewCheckPlan},
};

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_targets(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<AssignmentTarget<ScalarExpr>>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    { $read!(input.plan_target(memory, references)) }
                })
            }

            $($async)? fn plan_target(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<AssignmentTarget<ScalarExpr>>, SQLError> {
                let column = $read!(self.plan_text(memory))?;
                let indirection = plan_scalar_list!($read, self, memory, |input, memory| {
                    { $read!(input.plan_assignment_step(memory, references)) }
                })?;
                Ok(Pending::new(AssignmentTarget {
                    column,
                    indirection: indirection.take(),
                }))
            }

            $($async)? fn plan_assignment_step(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<AssignmentStep<ScalarExpr>>, SQLError> {
                Ok(Pending::new(
                    match $read!(self.binary.array::<1>())?[0] {
                        0 => AssignmentStep::Field($read!(self.plan_text(memory))?),
                        1 => {
                            memory.grow(size_of::<ScalarExpr>())?;
                            AssignmentStep::Index(Box::new(
                                $read!(self.plan_scalar(memory, references))?.take(),
                            ))
                        }
                        2 => {
                            let lower = $read!(self.plan_optional_scalar_box(memory, references))?;
                            let upper = $read!(self.plan_optional_scalar_box(memory, references))?;
                            AssignmentStep::Slice {
                                lower: lower.take(),
                                upper: upper.take(),
                            }
                        }
                        _ => return Err(invalid("query assignment step")),
                    },
                ))
            }

            $($async)? fn plan_assignment_targets(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<AssignmentTargets<ScalarExpr>>, SQLError> {
                Ok(Pending::new(
                    match $read!(self.binary.array::<1>())?[0] {
                        0 => AssignmentTargets::Single(
                            $read!(self.plan_target(memory, references))?.take(),
                        ),
                        1 => {
                            let targets = $read!(self.plan_targets(memory, references))?;
                            let source_positions =
                                input_list!($read, self, memory, |input, _| {
                                    Ok($read!(input.binary.count())?)
                                })?;
                            let source_width = $read!(self.binary.count())?;
                            if targets.len() != source_positions.len() {
                                return Err(invalid("multiple assignment address count"));
                            }
                            for position in &source_positions {
                                $read!(self.checkpoint())?;
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
                    },
                ))
            }

            pub(super) $($async)? fn plan_assignments(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<AssignmentPlan>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    {
                        let target = $read!(input.plan_assignment_targets(memory, references))?;
                        let value = $read!(input.plan_scalar(memory, references))?;
                        Ok(Pending::new(AssignmentPlan {
                            target: target.take(),
                            value: value.take(),
                        }))
                    }
                })
            }

            pub(super) $($async)? fn plan_conflict(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Option<ConflictPlan>>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(Pending::new(None));
                }
                let predicate = $read!(self.plan_optional_scalar_box(memory, references))?;
                let constraint = $read!(self.plan_optional_text(memory))?;
                let conflict_columns = $read!(self.plan_texts(memory))?;
                let expressions = $read!(self.plan_scalars(memory, references))?;
                let action = match $read!(self.binary.array::<1>())?[0] {
                    0 => ConflictActionPlan::Nothing,
                    1 => {
                        let assignments = $read!(self.plan_assignments(memory, references))?;
                        let predicate = $read!(self.plan_optional_scalar_box(memory, references))?;
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

            pub(super) $($async)? fn plan_view_checks(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<ViewCheckPlan>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    {
                        let view = $read!(input.plan_text(memory))?;
                        let predicate = $read!(input.plan_scalar(memory, references))?;
                        Ok(Pending::new(ViewCheckPlan {
                            view,
                            predicate: predicate.take(),
                        }))
                    }
                })
            }

            pub(super) $($async)? fn plan_when_clauses(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<MergeWhenPlan>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    $read!(input.plan_when(memory, references))
                })
            }

            $($async)? fn plan_when(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<MergeWhenPlan>, SQLError> {
                let tag = $read!(self.binary.array::<1>())?[0];
                if tag > 7 {
                    return Err(invalid("query merge action"));
                }
                let condition = $read!(self.plan_optional_scalar(memory, references))?;
                Ok(Pending::new(match tag {
                    0 | 2 => {
                        let assignments = $read!(self.plan_assignments(memory, references))?;
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
                        let columns = $read!(self.plan_targets(memory, references))?;
                        let overriding = $read!(self.plan_overriding())?;
                        let values = $read!(self.plan_scalars(memory, references))?;
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
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);
