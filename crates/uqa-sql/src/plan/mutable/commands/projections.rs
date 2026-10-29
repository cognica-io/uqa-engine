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
