//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule input retirement removes data before its constructor ownership and moves surviving assignment targets unchanged.

use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::{
    ast::{AssignmentTarget, AssignmentTargets},
    plan::{AssignmentPlan, ScalarExpr},
    production::Buffer,
};
use uqa_core::{memory::ProductionControl, Value};

pub(crate) struct OwnedAssignment {
    pub(crate) value: AssignmentPlan,
    pub(crate) memory: Option<RewriteMemory>,
}
pub(crate) struct OwnedTarget {
    pub(crate) value: AssignmentTarget<ScalarExpr>,
    pub(crate) memory: Option<RewriteMemory>,
}
struct Assignments {
    values: std::vec::IntoIter<AssignmentPlan>,
    memory: Option<RewriteMemory>,
    index: usize,
}
impl Iterator for Assignments {
    type Item = OwnedAssignment;
    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| memory.item_mut(self.index).take());
        self.index += 1;
        Some(OwnedAssignment { value, memory })
    }
}
struct Targets {
    values: std::vec::IntoIter<AssignmentTarget<ScalarExpr>>,
    _positions: Vec<usize>,
    memory: Option<RewriteMemory>,
    index: usize,
}
impl Iterator for Targets {
    type Item = OwnedTarget;
    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .field_mut("targets")
                .as_mut()
                .expect("group targets")
                .item_mut(self.index)
                .take()
        });
        self.index += 1;
        Some(OwnedTarget { value, memory })
    }
}

impl MutableCommandPlan {
    /// Remove a suppressed mutation's row source and query dependencies while retaining the capacities of cleared inline vectors.
    pub fn discard_rule_rows(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let body = self
            .memory
            .as_mut()
            .map(|memory| memory.field_mut("value0").as_mut().expect("mutation body"));
        match &mut self.command {
            CommandPlan::Insert(plan) => {
                plan.source = None;
                plan.ctes.clear();
                plan.subqueries.clear();
                if let Some(body) = body {
                    *body.field_mut("source") = None;
                    clear_items(body, "ctes");
                    clear_items(body, "subqueries");
                }
            }
            CommandPlan::Update(plan) => {
                plan.source = None;
                plan.predicate = None;
                plan.ctes.clear();
                plan.subqueries.clear();
                if let Some(body) = body {
                    *body.field_mut("source") = None;
                    *body.field_mut("predicate") = None;
                    clear_items(body, "ctes");
                    clear_items(body, "subqueries");
                }
            }
            CommandPlan::Delete(plan) => {
                plan.source = None;
                plan.predicate = None;
                plan.ctes.clear();
                plan.subqueries.clear();
                if let Some(body) = body {
                    *body.field_mut("source") = None;
                    *body.field_mut("predicate") = None;
                    clear_items(body, "ctes");
                    clear_items(body, "subqueries");
                }
            }
            _ => {
                return Err(SQLError::Internal(
                    "rule row retirement requires INSERT, UPDATE or DELETE".into(),
                ))
            }
        }
        Ok(self)
    }

    /// Split wholly unused grouped assignments into their surviving single target descriptors and replace every discarded bound/value with NULL.
    pub fn prune_rule_assignments(
        mut self,
        required: &dyn Fn(&str) -> bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Update(update) = &self.command else {
            return Err(SQLError::Internal("rule assignments require UPDATE".into()));
        };
        let count = update.assignments.len();
        for index in 0..count {
            let CommandPlan::Update(update) = &self.command else {
                unreachable!()
            };
            if update.assignments[index]
                .target
                .column_names()
                .all(|name| !required(name))
            {
                self = self.map_assignment(
                    super::CommandAssignmentSlot::Update(index),
                    control,
                    |old| {
                        drop(old);
                        Ok(ScalarExpr::Literal(Value::Null).copy_for_rewrite(control)?)
                    },
                )?;
            }
        }
        let CommandPlan::Update(update) = &mut self.command else {
            unreachable!()
        };
        let values = std::mem::take(&mut update.assignments);
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .field_mut("value0")
                .as_mut()
                .expect("update body")
                .field_mut("assignments")
                .take()
        });
        let values = Assignments {
            values: values.into_iter(),
            memory,
            index: 0,
        };
        let mut output = Buffer::new(control);
        for mut assignment in values {
            control.check()?;
            if assignment.value.target.column_names().any(required)
                || matches!(assignment.value.target, AssignmentTargets::Single(_))
            {
                output.push(assignment, control)?;
                continue;
            }
            let AssignmentTargets::Multiple(group) = assignment.value.target else {
                unreachable!()
            };
            let memory = assignment
                .memory
                .as_mut()
                .and_then(|memory| memory.field_mut("target").take());
            let targets = Targets {
                values: group.targets.into_iter(),
                _positions: group.source_positions,
                memory,
                index: 0,
            };
            drop(assignment.value.value);
            drop(assignment.memory);
            for target in targets {
                output.push(
                    crate::plan::copy::null_assignment(target, control)?,
                    control,
                )?;
            }
        }
        let (values, memory) = crate::plan::copy::assignment_vector(output.into_iter(), control)?;
        let CommandPlan::Update(update) = &mut self.command else {
            unreachable!()
        };
        update.assignments = values;
        if let Some(owner) = &mut self.memory {
            *owner
                .field_mut("value0")
                .as_mut()
                .expect("update body")
                .field_mut("assignments") = memory;
        }
        Ok(self)
    }
}
fn clear_items(memory: &mut RewriteMemory, field: &str) {
    let RewriteMemory::Fields { fields, .. } =
        memory.field_mut(field).as_mut().expect("retained vector")
    else {
        unreachable!("vector owner")
    };
    fields.clear();
}
