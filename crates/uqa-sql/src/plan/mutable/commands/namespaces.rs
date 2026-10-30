//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct command expressions lend their existing query namespace to consuming replacements.

use super::{CommandAssignmentSlot, CommandPlan, CommandScalarSlot, MutableCommandPlan, SQLError};
use crate::plan::{OwnedScalarExpr, ScalarQueryArena};
use uqa_core::memory::ProductionControl;

impl MutableCommandPlan {
    pub fn map_scalar_input(
        mut self,
        slot: CommandScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedScalarExpr, &mut ScalarQueryArena) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_mutation_arena()?;
        self = self.map_scalar(slot, control, |input| map(input, &mut arena))?;
        self.restore_mutation_arena(arena)?;
        Ok(self)
    }

    pub fn map_assignment_input(
        mut self,
        slot: CommandAssignmentSlot,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(OwnedScalarExpr, &mut ScalarQueryArena) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_mutation_arena()?;
        self = self.map_assignment(slot, control, |input| map(input, &mut arena))?;
        self.restore_mutation_arena(arena)?;
        Ok(self)
    }

    pub fn map_mutation_arena(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&CommandPlan, &mut ScalarQueryArena) -> Result<(), SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_mutation_arena()?;
        map(&self.command, &mut arena)?;
        control.check()?;
        self.restore_mutation_arena(arena)?;
        Ok(self)
    }

    pub(super) fn take_mutation_arena(&mut self) -> Result<ScalarQueryArena, SQLError> {
        let queries = std::mem::take(mutation_queries(&mut self.command)?);
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("subqueries")
                .take()
        });
        Ok(ScalarQueryArena { queries, memory })
    }

    pub(super) fn restore_mutation_arena(
        &mut self,
        arena: ScalarQueryArena,
    ) -> Result<(), SQLError> {
        *mutation_queries(&mut self.command)? = arena.queries;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("subqueries") = arena.memory;
        }
        Ok(())
    }
}

fn mutation_queries(
    command: &mut CommandPlan,
) -> Result<&mut Vec<crate::plan::QueryPlan>, SQLError> {
    match command {
        CommandPlan::Insert(plan) => Ok(&mut plan.subqueries),
        CommandPlan::Update(plan) => Ok(&mut plan.subqueries),
        CommandPlan::Delete(plan) => Ok(&mut plan.subqueries),
        CommandPlan::Merge(plan) => Ok(&mut plan.subqueries),
        _ => Err(SQLError::Internal(
            "scalar query namespace requires a mutation".into(),
        )),
    }
}
