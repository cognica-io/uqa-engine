//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{invalid, retirement::Pending, Fields, Input, PlanMemory, SQLError};
use crate::{
    ast::WindowDefinition, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_windows(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<WindowDefinition<ScalarWindowSpec>>>, SQLError> {
                let mut position = 0;
                plan_scalar_list!($read, self, memory, |input, memory| {
                    {
                        let name = $read!(input.plan_optional_text(memory))?;
                        let inherited = $read!(input.plan_optional_count())?;
                        if inherited.is_some_and(|index| index >= position) {
                            return Err(invalid("query window inheritance address"));
                        }
                        let spec = $read!(input.plan_window(memory, references))?;
                        position += 1;
                        Ok(Pending::new(WindowDefinition {
                            name,
                            inherited,
                            spec: spec.take(),
                        }))
                    }
                })
            }

            $($async)? fn plan_window(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<ScalarWindowSpec>, SQLError> {
                let definition = $read!(self.plan_optional_count())?;
                references.window = references.window.max(definition);
                let partition_by = $read!(self.plan_scalars(memory, references))?;
                let order_by = plan_scalar_list!($read, self, memory, |input, memory| {
                    {
                        let expr = $read!(input.plan_scalar(memory, references))?;
                        let descending = $read!(input.flag())?;
                        let nulls = $read!(input.plan_optional_nulls())?;
                        Ok(Pending::new(ScalarOrder {
                            expr: expr.take(),
                            descending,
                            nulls,
                        }))
                    }
                })?;
                let frame = if $read!(self.flag())? {
                    Pending::new(Some(
                        $read!(self.plan_frame(memory, references))?.take(),
                    ))
                } else {
                    Pending::new(None)
                };
                Ok(Pending::new(ScalarWindowSpec {
                    definition,
                    partition_by: partition_by.take(),
                    order_by: order_by.take(),
                    frame: frame.take(),
                }))
            }

            $($async)? fn plan_frame(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<ScalarWindowFrame>, SQLError> {
                let mode = $read!(self.plan_frame_mode())?;
                let start = $read!(self.plan_frame_bound(memory, references))?;
                let end = $read!(self.plan_frame_bound(memory, references))?;
                let between = $read!(self.flag())?;
                let exclusion = $read!(self.plan_exclusion())?;
                Ok(Pending::new(ScalarWindowFrame {
                    mode,
                    start: start.take(),
                    end: end.take(),
                    between,
                    exclusion,
                }))
            }

            $($async)? fn plan_frame_bound(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<ScalarFrameBound>, SQLError> {
                let tag = $read!(self.binary.array::<1>())?[0];
                let value = match tag {
                    0 => ScalarFrameBound::UnboundedPreceding,
                    1 => ScalarFrameBound::UnboundedFollowing,
                    2 => ScalarFrameBound::CurrentRow,
                    3 | 4 => {
                        memory.grow(size_of::<crate::ScalarExpr>())?;
                        let expr = Box::new($read!(self.plan_scalar(memory, references))?.take());
                        if tag == 3 {
                            ScalarFrameBound::Preceding(expr)
                        } else {
                            ScalarFrameBound::Following(expr)
                        }
                    }
                    _ => return Err(invalid("query window frame bound")),
                };
                Ok(Pending::new(value))
            }
        }
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);
