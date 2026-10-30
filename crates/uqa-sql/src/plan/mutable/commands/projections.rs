//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RETURNING names retain their independently admitted payload when scalar values change.

use super::{item, CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl MutableCommandPlan {
    pub fn replace_returning_alias(
        mut self,
        index: usize,
        alias: Option<Produced<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let projections = match &mut self.command {
            CommandPlan::Insert(plan) => &mut plan.returning,
            CommandPlan::Update(plan) => &mut plan.returning,
            CommandPlan::Delete(plan) => &mut plan.returning,
            CommandPlan::Merge(plan) => &mut plan.returning,
            _ => {
                return Err(SQLError::Internal(
                    "RETURNING alias requires a mutation".into(),
                ));
            }
        };
        let (alias, reservation) = match alias {
            Some(alias) => {
                let (alias, memory) = alias.into_parts();
                (Some(alias), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let (alias, reservation) = control.finish(alias, reservation)?.into_parts();
        let memory = reservation.map(|direct| RewriteMemory::Fields {
            fields: BudgetedVec::new(direct.budget()),
            direct,
        });
        projections[index].alias = alias;
        if let Some(owner) = &mut self.memory {
            let body = owner.field_mut("value0").as_mut().expect("mutation body");
            *item(body, "returning", index).field_mut("alias") = memory;
        }
        Ok(self)
    }
}

impl MutableCommandPlan {
    /// Rewrite RETURNING projections and append embedded queries to the same admitted arena.
    pub fn map_returning_inputs<I: ExactSizeIterator<Item = crate::plan::OwnedProjection>>(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(
            &CommandPlan,
            crate::plan::OwnedProjectionIter,
            &mut crate::plan::ScalarQueryArena,
        ) -> Result<I, SQLError>,
    ) -> Result<Self, SQLError> {
        use super::super::projections::{OwnedProjectionIter, ProjectionVector};
        self.assert_owner(control);
        control.check()?;
        let projections = std::mem::take(returning(&mut self.command)?).into_iter();
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("returning")
                .take()
        });
        let inputs = OwnedProjectionIter {
            projections,
            memory,
            index: 0,
        };
        let mut arena = self.take_mutation_arena()?;
        let replacement = ProjectionVector::new(map(&self.command, inputs, &mut arena)?, control)?;
        *returning(&mut self.command)? = replacement.projections;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("returning") = replacement.memory;
        }
        self.restore_mutation_arena(arena)?;
        control.check()?;
        Ok(self)
    }
}

fn returning(command: &mut CommandPlan) -> Result<&mut Vec<crate::plan::ProjectionPlan>, SQLError> {
    match command {
        CommandPlan::Insert(plan) => Ok(&mut plan.returning),
        CommandPlan::Update(plan) => Ok(&mut plan.returning),
        CommandPlan::Delete(plan) => Ok(&mut plan.returning),
        CommandPlan::Merge(plan) => Ok(&mut plan.returning),
        _ => Err(SQLError::Internal("RETURNING requires a mutation".into())),
    }
}
