//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{
    driver::Step, invalid, retirement::Pending, Input, Kind, MemoryReservation, Node, SQLError,
};
use crate::plan::{CommandPlan, CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan, QueryPlan};

pub(super) fn read(
    value: &mut CtePlan,
    position: usize,
    references: &mut References,
    input: &mut Input<'_>,
    memory: &mut MemoryReservation,
) -> Result<Step, SQLError> {
    match position {
        0 => value.name = input.plan_text(memory)?,
        1 => value.columns = input.plan_texts(memory)?,
        2 => value.recursive = input.flag()?,
        3 => value.materialization = input.plan_materialization()?,
        4 => {
            value.search = if input.flag()? {
                Some(CteSearchPlan {
                    columns: input.plan_texts(memory)?,
                    breadth_first: input.flag()?,
                    sequence_column: input.plan_text(memory)?,
                })
            } else {
                None
            }
        }
        5 => value.cycle = input.plan_cycle(memory, references)?.take(),
        6 => {
            return match input.binary.array::<1>()?[0] {
                0 => Ok(Step::Child(Kind::Query)),
                1 => {
                    memory.grow(size_of::<CommandPlan>())?;
                    value.body = CtePlanBody::Command(Box::new(CommandPlan::Empty));
                    drop(memory.split(size_of::<QueryPlan>()));
                    Ok(Step::Child(Kind::Command))
                }
                _ => Err(invalid("query CTE body constructor")),
            }
        }
        _ => return Ok(Step::Done),
    }
    Ok(Step::Advance)
}

impl Input<'_> {
    fn plan_cycle(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<Option<CteCyclePlan>>, SQLError> {
        if !self.flag()? {
            return Ok(Pending::new(None));
        }
        let columns = self.plan_texts(memory)?;
        let mark_column = self.plan_text(memory)?;
        let mark_value = self.plan_scalar(memory, references)?;
        let mark_default = self.plan_scalar(memory, references)?;
        let path_column = self.plan_text(memory)?;
        Ok(Pending::new(Some(CteCyclePlan {
            columns,
            mark_column,
            mark_value: mark_value.take(),
            mark_default: mark_default.take(),
            path_column,
        })))
    }
}

pub(super) fn attach(value: &mut CtePlan, position: &mut usize, child: Node) {
    match (&mut value.body, child) {
        (CtePlanBody::Query(target), Node::Query(query)) => **target = query,
        (CtePlanBody::Command(target), Node::Command(command)) => **target = command,
        _ => unreachable!("CTE body constructor"),
    }
    *position += 1;
}
