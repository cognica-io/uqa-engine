//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, Kind, PlanMemory, References, SQLError, Step};
use crate::plan::{RuleValuesInput, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan};

pub(super) fn returning(
    value: &mut Option<ViewRuleReturningPlan>,
    count: &mut Option<usize>,
    references: &mut References,
    input: &mut Input<'_>,
    memory: &mut PlanMemory,
) -> Result<Step, SQLError> {
    if count.is_none() {
        if !input.flag()? {
            return Ok(Step::Advance);
        }
        *value = Some(ViewRuleReturningPlan {
            relation: String::new(),
            target_qualifier: String::new(),
            returning: Vec::new(),
            aliases: super::constructors::aliases(),
            subqueries: Vec::new(),
        });
        let value = value.as_mut().expect("owned rule returning input");
        value.relation = input.plan_text(memory)?;
        value.target_qualifier = input.plan_text(memory)?;
        value.returning = input.plan_projections(memory, references)?.take();
        value.aliases = input.plan_returning_aliases(memory)?;
    }
    input.plan_children(
        memory,
        count,
        &mut value
            .as_mut()
            .expect("rule returning query arena")
            .subqueries,
        Kind::Query,
    )
}

impl Input<'_> {
    pub(in super::super) fn plan_rule_values(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<RuleValuesInput>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let width = self.binary.count()?;
        let inline = self.flag()?;
        let defaults = self.plan_vector(memory, |input, _| Ok(input.binary.count()?))?;
        let unresolved = self.plan_positions(memory)?;
        let written = self.plan_positions(memory)?;
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

    pub(in super::super) fn plan_rule_inserts(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Vec<ViewRuleInsertPlan>, SQLError> {
        self.plan_vector(memory, |input, memory| {
            Ok(ViewRuleInsertPlan {
                relation: input.plan_text(memory)?,
                supplied_columns: input.plan_texts(memory)?,
                input_columns: input.plan_texts(memory)?,
            })
        })
    }

    pub(in super::super) fn plan_rule_updates(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Vec<ViewRuleUpdatePlan>, SQLError> {
        self.plan_vector(memory, |input, memory| {
            Ok(ViewRuleUpdatePlan {
                relation: input.plan_text(memory)?,
                assigned_columns: input.plan_texts(memory)?,
                input_columns: input.plan_texts(memory)?,
            })
        })
    }
}
