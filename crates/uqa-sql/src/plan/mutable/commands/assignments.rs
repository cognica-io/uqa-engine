//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment bounds and values retain the traversal order of their independent scalar owners.

use super::{
    item, CommandPlan, MutableCommandPlan, OwnedScalarExpr, RewriteMemory, SQLError, ScalarExpr,
};
use crate::{
    ast::{AssignmentStep, AssignmentTargets},
    plan::{AssignmentPlan, ConflictActionPlan, MergeWhenPlan},
};
use uqa_core::{memory::ProductionControl, Value};

#[derive(Clone, Copy, Debug)]
pub enum CommandAssignmentSlot {
    Update(usize),
    Conflict(usize),
    Merge { clause: usize, assignment: usize },
}

impl MutableCommandPlan {
    /// Rewrite every bound followed by the value of one assignment, preserving grouped-target order and admitting each replacement independently.
    pub fn map_assignment(
        mut self,
        slot: CommandAssignmentSlot,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let assignment = slot.assignment_mut(&mut self.command);
        let mut memory = self.memory.as_mut().map(|memory| slot.memory_mut(memory));
        let target_memory = memory.as_mut().map(|memory| {
            memory
                .field_mut("target")
                .as_mut()
                .expect("assignment target")
        });
        match &mut assignment.target {
            AssignmentTargets::Single(target) => {
                rewrite_target(target, target_memory, control, &mut map)?;
            }
            AssignmentTargets::Multiple(group) => {
                let mut target_memory = target_memory;
                for (index, target) in group.targets.iter_mut().enumerate() {
                    let memory = target_memory
                        .as_mut()
                        .map(|memory| item(memory, "targets", index));
                    rewrite_target(target, memory, control, &mut map)?;
                }
            }
        }
        let memory = memory.map(|memory| {
            memory
                .field_mut("value")
                .as_mut()
                .expect("assignment value")
                .scalar_mut()
        });
        rewrite_scalar(&mut assignment.value, memory, control, &mut map)?;
        Ok(self)
    }
}

fn rewrite_target(
    target: &mut crate::ast::AssignmentTarget<ScalarExpr>,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    map: &mut impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<(), SQLError> {
    for (index, step) in target.indirection.iter_mut().enumerate() {
        let mut memory = memory
            .as_mut()
            .map(|memory| item(memory, "indirection", index));
        match step {
            AssignmentStep::Field(_) => {}
            AssignmentStep::Index(expression) => rewrite_scalar(
                expression,
                memory.map(|memory| {
                    memory
                        .field_mut("index")
                        .as_mut()
                        .expect("index box")
                        .scalar_mut()
                }),
                control,
                map,
            )?,
            AssignmentStep::Slice { lower, upper } => {
                if let Some(expression) = lower {
                    rewrite_scalar(
                        expression,
                        memory.as_mut().map(|memory| {
                            memory
                                .field_mut("lower")
                                .as_mut()
                                .expect("lower bound")
                                .scalar_mut()
                        }),
                        control,
                        map,
                    )?;
                }
                if let Some(expression) = upper {
                    rewrite_scalar(
                        expression,
                        memory.map(|memory| {
                            memory
                                .field_mut("upper")
                                .as_mut()
                                .expect("upper bound")
                                .scalar_mut()
                        }),
                        control,
                        map,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn rewrite_scalar(
    expression: &mut ScalarExpr,
    mut memory: Option<&mut Option<RewriteMemory>>,
    control: &ProductionControl<'_>,
    map: &mut impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    let retained = memory
        .as_mut()
        .and_then(|memory| memory.take())
        .map(|memory| match memory {
            RewriteMemory::Scalar(memory) => memory,
            RewriteMemory::Fields { .. } => unreachable!("assignment scalar owner"),
        });
    let input = OwnedScalarExpr::from_rewrite_parts(
        std::mem::replace(expression, ScalarExpr::Literal(Value::Null)),
        retained,
    );
    let output = input.rewrite(control, map)?;
    let (output, owner) = output.into_rewrite_parts();
    *expression = output;
    if let Some(memory) = memory {
        *memory = owner.map(RewriteMemory::Scalar);
    }
    Ok(())
}

impl CommandAssignmentSlot {
    fn assignment_mut(self, command: &mut CommandPlan) -> &mut AssignmentPlan {
        match (self, command) {
            (Self::Update(index), CommandPlan::Update(plan)) => &mut plan.assignments[index],
            (Self::Conflict(index), CommandPlan::Insert(plan)) => {
                let ConflictActionPlan::Update { assignments, .. } =
                    &mut plan.on_conflict.as_mut().expect("conflict action").action
                else {
                    panic!("conflict has assignments");
                };
                &mut assignments[index]
            }
            (Self::Merge { clause, assignment }, CommandPlan::Merge(plan)) => {
                match &mut plan.when_clauses[clause] {
                    MergeWhenPlan::UpdateMatched { assignments, .. }
                    | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                        &mut assignments[assignment]
                    }
                    _ => panic!("MERGE clause has assignments"),
                }
            }
            _ => panic!("assignment selector matches command kind"),
        }
    }

    fn memory_mut(self, memory: &mut RewriteMemory) -> &mut RewriteMemory {
        let body = memory.field_mut("value0").as_mut().expect("mutation body");
        match self {
            Self::Update(index) => item(body, "assignments", index),
            Self::Conflict(index) => item(
                body.field_mut("on_conflict")
                    .as_mut()
                    .expect("conflict")
                    .field_mut("action")
                    .as_mut()
                    .expect("conflict action"),
                "assignments",
                index,
            ),
            Self::Merge { clause, assignment } => item(
                item(body, "when_clauses", clause),
                "assignments",
                assignment,
            ),
        }
    }
}
