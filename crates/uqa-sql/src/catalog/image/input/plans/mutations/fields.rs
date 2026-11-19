//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Fields, Input, Kind, PlanMemory, References, SQLError, Step};
use crate::plan::{RuleValuesInput, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan};

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(in super::super) $($async)? fn plan_rule_returning(
                &mut self,
                value: &mut Option<ViewRuleReturningPlan>,
                count: &mut Option<usize>,
                references: &mut References,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                if count.is_none() {
                    if !$read!(self.flag())? {
                        return Ok(Step::Advance);
                    }
                    *value = Some(ViewRuleReturningPlan {
                        relation: String::new(),
                        target_qualifier: String::new(),
                        returning: Vec::new(),
                        aliases: super::constructors::aliases(),
                        subqueries: Vec::new(),
                    });
                    let value = value.as_mut().expect("owned rule returning self");
                    value.relation = $read!(self.plan_text(memory))?;
                    value.target_qualifier = $read!(self.plan_text(memory))?;
                    value.returning = $read!(self.plan_projections(memory, references))?.take();
                    value.aliases = $read!(self.plan_returning_aliases(memory))?;
                }
                $read!(
                    self.plan_children(
                        memory,
                        count,
                        &mut value
                            .as_mut()
                            .expect("rule returning query arena")
                            .subqueries,
                        Kind::Query,
                    )
                )
            }

            pub(in super::super) $($async)? fn plan_rule_values(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<Option<RuleValuesInput>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(None);
                }
                let width = $read!(self.binary.count())?;
                let inline = $read!(self.flag())?;
                let defaults = input_list!($read, self, memory, |input, _| {
                    Ok($read!(input.binary.count())?)
                })?;
                let unresolved = $read!(self.plan_positions(memory))?;
                let written = $read!(self.plan_positions(memory))?;
                if width
                    .checked_add(if inline { 0 } else { defaults.len() })
                    .is_none()
                {
                    return Err(super::super::invalid("query VALUES source width"));
                }
                if defaults.iter().any(|position| *position >= width) {
                    return Err(super::super::invalid("query VALUES default position"));
                }
                Ok(Some(RuleValuesInput {
                    width,
                    inline,
                    defaults,
                    unresolved,
                    written,
                }))
            }

            pub(in super::super) $($async)? fn plan_rule_inserts(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<Vec<ViewRuleInsertPlan>, SQLError> {
                input_list!($read, self, memory, |input, memory| {
                    {
                        Ok(ViewRuleInsertPlan {
                            relation: $read!(input.plan_text(memory))?,
                            supplied_columns: $read!(input.plan_texts(memory))?,
                            input_columns: $read!(input.plan_texts(memory))?,
                        })
                    }
                })
            }

            pub(in super::super) $($async)? fn plan_rule_updates(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<Vec<ViewRuleUpdatePlan>, SQLError> {
                input_list!($read, self, memory, |input, memory| {
                    {
                        Ok(ViewRuleUpdatePlan {
                            relation: $read!(input.plan_text(memory))?,
                            assigned_columns: $read!(input.plan_texts(memory))?,
                            input_columns: $read!(input.plan_texts(memory))?,
                        })
                    }
                })
            }
        }
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);
