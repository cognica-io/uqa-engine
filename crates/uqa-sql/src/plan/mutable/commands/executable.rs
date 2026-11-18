//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Utility statement bodies keep their existing wrapper and statement boxes while moving the executable owner.

use super::super::{
    edits::{BodySlot, Slot},
    MutableExecutablePlan,
};
use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
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
        let replacement = memory.map(RewriteMemory::direct);
        *unbound_parameters = parameters;
        if let Some(memory) = self.memory.as_mut() {
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
        let mut slot = self.executable_slot()?;
        let output = map(slot.take())?;
        slot.complete(output, control, "utility")?;
        Ok(self)
    }

    pub(in crate::plan::mutable) fn executable_slot(&mut self) -> Result<BodySlot<'_>, SQLError> {
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
        Ok(match body.as_mut() {
            UnifiedPlan::Query(query) => BodySlot::Query(Slot::new(
                query.as_mut(),
                memory.map(|memory| memory.field_mut("query")),
            )),
            UnifiedPlan::Command(command) => BodySlot::Command(Slot::new(
                command.as_mut(),
                memory.map(|memory| memory.field_mut("command")),
            )),
        })
    }
}
