//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{
    driver::Step, invalid, retirement::Pending, Fields, Input, Kind, Node, PlanMemory, SQLError,
};
use crate::plan::{CommandPlan, CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan, QueryPlan};

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_cte_fields(
                &mut self,
                value: &mut CtePlan,
                position: usize,
                references: &mut References,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                match position {
                    0 => value.name = $read!(self.plan_text(memory))?,
                    1 => value.columns = $read!(self.plan_texts(memory))?,
                    2 => value.recursive = $read!(self.flag())?,
                    3 => value.materialization = $read!(self.plan_materialization())?,
                    4 => {
                        value.search = if $read!(self.flag())? {
                            Some(CteSearchPlan {
                                columns: $read!(self.plan_texts(memory))?,
                                breadth_first: $read!(self.flag())?,
                                sequence_column: $read!(self.plan_text(memory))?,
                            })
                        } else {
                            None
                        }
                    }
                    5 => value.cycle = $read!(self.plan_cycle(memory, references))?.take(),
                    6 => {
                        return match $read!(self.binary.array::<1>())?[0] {
                            0 => Ok(Step::Child(Kind::Query)),
                            1 => {
                                memory.grow(size_of::<CommandPlan>())?;
                                value.body = CtePlanBody::Command(Box::new(CommandPlan::Empty));
                                drop(memory.split(size_of::<QueryPlan>()));
                                Ok(Step::Child(Kind::Command))
                            }
                            _ => Err(invalid("query CTE body constructor")),
                        };
                    }
                    _ => return Ok(Step::Done),
                }
                Ok(Step::Advance)
            }

            $($async)? fn plan_cycle(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Option<CteCyclePlan>>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(Pending::new(None));
                }
                let columns = $read!(self.plan_texts(memory))?;
                let mark_column = $read!(self.plan_text(memory))?;
                let mark_value = $read!(self.plan_scalar(memory, references))?;
                let mark_default = $read!(self.plan_scalar(memory, references))?;
                let path_column = $read!(self.plan_text(memory))?;
                Ok(Pending::new(Some(CteCyclePlan {
                    columns,
                    mark_column,
                    mark_value: mark_value.take(),
                    mark_default: mark_default.take(),
                    path_column,
                })))
            }
        }
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);

pub(super) fn attach(value: &mut CtePlan, position: &mut usize, child: Node) {
    match (&mut value.body, child) {
        (CtePlanBody::Query(target), Node::Query(query)) => **target = query,
        (CtePlanBody::Command(target), Node::Command(command)) => **target = command,
        _ => unreachable!("CTE body constructor"),
    }
    *position += 1;
}
