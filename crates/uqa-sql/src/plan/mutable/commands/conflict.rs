//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final inference publication moves simple attributes into the conflict name list with their existing payload leases.

use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError, ScalarExpr};
use crate::plan::ConflictPlan;
use uqa_core::{
    memory::{BudgetedVec, Produced, ProductionControl, ProductionVec},
    Value,
};

impl MutableCommandPlan {
    /// Finish inference normalization after all expression edits. Existing names retain their order; newly simple columns follow in expression order, including duplicates.
    pub fn finish_inference_columns(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CommandPlan>, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "inference publication requires INSERT".into(),
            ));
        };
        if let Some(conflict) = &mut plan.on_conflict {
            if conflict
                .expressions
                .iter()
                .any(|expression| matches!(expression, ScalarExpr::Column(_)))
            {
                let owner = self.memory.as_mut().map(|memory| {
                    memory
                        .field_mut("value0")
                        .as_mut()
                        .expect("INSERT body")
                        .field_mut("on_conflict")
                        .as_mut()
                        .expect("conflict")
                });
                promote_columns(conflict, owner, control)?;
            }
        }
        self.finish(control)
    }
}

fn promote_columns(
    conflict: &mut ConflictPlan,
    mut owner: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut names = ProductionVec::new(*control);
    // The command's original vector owner remains live until this iterator releases its buffer.
    for (index, name) in std::mem::take(&mut conflict.conflict_columns)
        .into_iter()
        .enumerate()
    {
        let memory = owner
            .as_mut()
            .and_then(|owner| {
                owner
                    .field_mut("conflict_columns")
                    .as_mut()
                    .expect("conflict names")
                    .item_mut(index)
                    .take()
            })
            .map(RewriteMemory::into_reservation);
        names.push_produced(control.finish(name, memory)?)?;
    }
    let mut retained = 0;
    for index in 0..conflict.expressions.len() {
        control.check()?;
        if matches!(conflict.expressions[index], ScalarExpr::Column(_)) {
            let ScalarExpr::Column(name) = std::mem::replace(
                &mut conflict.expressions[index],
                ScalarExpr::Literal(Value::Null),
            ) else {
                unreachable!("simple inference attribute")
            };
            let memory = owner
                .as_mut()
                .and_then(|owner| {
                    owner
                        .field_mut("expressions")
                        .as_mut()
                        .expect("inference expressions")
                        .item_mut(index)
                        .as_mut()
                        .expect("inference scalar")
                        .scalar_mut()
                        .take()
                })
                .map(RewriteMemory::into_reservation);
            names.push_produced(control.finish(String::from(name), memory)?)?;
        } else {
            conflict.expressions.swap(retained, index);
            if let Some(owner) = &mut owner {
                let RewriteMemory::Fields { fields, .. } = owner
                    .field_mut("expressions")
                    .as_mut()
                    .expect("inference expressions")
                else {
                    unreachable!("expression vector owner")
                };
                fields.swap(retained, index);
            }
            retained += 1;
        }
    }
    conflict.expressions.truncate(retained);
    if retained == 0 {
        conflict.expressions = Vec::new();
    }
    if let Some(owner) = &mut owner {
        let RewriteMemory::Fields { fields, .. } = owner
            .field_mut("expressions")
            .as_mut()
            .expect("inference expressions")
        else {
            unreachable!("expression vector owner")
        };
        fields.truncate(retained);
        if retained == 0 {
            *owner.field_mut("expressions") =
                control
                    .empty_reservation()
                    .map(|direct| RewriteMemory::Fields {
                        fields: BudgetedVec::new(direct.budget()),
                        direct,
                    });
        }
    }
    let (names, memory) = names.finish()?.into_parts();
    conflict.conflict_columns = names;
    if let Some(owner) = owner {
        // No further field edit follows this finalization; publication flattens the name buffer's owner with the complete command.
        *owner.field_mut("conflict_columns") = memory.map(|direct| RewriteMemory::Fields {
            fields: BudgetedVec::new(direct.budget()),
            direct,
        });
    }
    Ok(())
}
