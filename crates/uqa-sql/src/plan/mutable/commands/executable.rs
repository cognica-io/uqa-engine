//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Utility statement bodies keep their existing wrapper and statement boxes while moving the executable owner.

use super::super::MutableExecutablePlan;
use super::{CommandPlan, MutableCommandPlan, MutableQueryPlan, RewriteMemory, SQLError};
use crate::plan::UnifiedPlan;
use uqa_core::memory::ProductionControl;

impl MutableCommandPlan {
    /// Replace only EXPLAIN's inferred parameter owner after its body analysis succeeds.
    pub fn set_explain_parameters(
        mut self,
        parameters: Option<uqa_core::memory::Produced<Vec<crate::SQLParam>>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Explain {
            unbound_parameters, ..
        } = &mut self.command
        else {
            return Err(SQLError::Internal(
                "inferred parameters require EXPLAIN".into(),
            ));
        };
        let (parameters, memory) = match parameters {
            Some(parameters) => {
                let (parameters, memory) = parameters.into_parts();
                (Some(parameters), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let (parameters, memory) = control.finish(parameters, memory)?.into_parts();
        let replacement = memory.map(|direct| RewriteMemory::Fields {
            fields: uqa_core::memory::BudgetedVec::new(direct.budget()),
            direct,
        });
        *unbound_parameters = parameters;
        if let Some(memory) = &mut self.memory {
            *memory.field_mut("unbound_parameters") = replacement;
        }
        Ok(self)
    }
    /// Rewrite EXPLAIN or PREPARE's complete nested statement without moving its wrapper metadata or changing the query/command variant.
    pub fn map_executable_body(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let body = match &mut self.command {
            CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => body,
            _ => {
                return Err(SQLError::Internal(
                    "executable body requires EXPLAIN or PREPARE".into(),
                ))
            }
        };
        let memory = self.memory.as_mut().map(|memory| {
            memory
                .field_mut("body")
                .as_mut()
                .expect("utility body owner")
        });
        rewrite(body, memory, control, map)?;
        Ok(self)
    }
}

fn rewrite(
    body: &mut UnifiedPlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    map: impl FnOnce(MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError>,
) -> Result<(), SQLError> {
    match body {
        UnifiedPlan::Query(query) => {
            let input = MutableQueryPlan {
                query: std::mem::replace(query.as_mut(), super::super::queries::empty_query()),
                memory: memory
                    .as_mut()
                    .and_then(|memory| memory.field_mut("query").take()),
            };
            let MutableExecutablePlan::Query(output) = map(MutableExecutablePlan::Query(input))?
            else {
                return Err(SQLError::Internal(
                    "utility rewrite changed a query body into a command".into(),
                ));
            };
            output.assert_owner(control);
            control.check()?;
            **query = output.query;
            if let Some(memory) = memory {
                *memory.field_mut("query") = output.memory;
            }
        }
        UnifiedPlan::Command(command) => {
            let input = MutableCommandPlan {
                command: std::mem::replace(command.as_mut(), CommandPlan::Empty),
                memory: memory
                    .as_mut()
                    .and_then(|memory| memory.field_mut("command").take()),
            };
            let MutableExecutablePlan::Command(output) =
                map(MutableExecutablePlan::Command(input))?
            else {
                return Err(SQLError::Internal(
                    "utility rewrite changed a command body into a query".into(),
                ));
            };
            output.assert_owner(control);
            control.check()?;
            **command = output.command;
            if let Some(memory) = memory {
                *memory.field_mut("command") = output.memory;
            }
        }
    }
    Ok(())
}
