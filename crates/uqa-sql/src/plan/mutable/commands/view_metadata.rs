//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View checks and rule input descriptors retain independent vector and child constructors.

use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::{
    plan::mutable::field_vectors::{FieldVector, OwnedFields},
    plan::{
        copy::Copying, OwnedScalarExpr, ScalarQueryArena, ViewCheckPlan, ViewRuleInsertPlan,
        ViewRuleUpdatePlan,
    },
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl, ProductionVec};

impl MutableCommandPlan {
    pub fn prepend_view_check(
        mut self,
        view: Produced<String>,
        control: &ProductionControl<'_>,
        predicate: impl FnOnce(&mut ScalarQueryArena) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_mutation_arena()?;
        let predicate = predicate(&mut arena)?;
        self.restore_mutation_arena(arena)?;
        let incoming = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let value = copying.owned_view_check(view, predicate)?;
            OwnedFields {
                value,
                memory: Some(copying.into_rewrite_memory()),
            }
        } else {
            OwnedFields {
                value: ViewCheckPlan {
                    view: view.into_uncontrolled().expect("ordinary check name"),
                    predicate: predicate
                        .finish(control)?
                        .into_uncontrolled()
                        .expect("ordinary check predicate"),
                },
                memory: None,
            }
        };
        let values = std::mem::take(checks(&mut self.command)?);
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| body(memory).field_mut("view_checks").take());
        let replacement = FieldVector { values, memory }.insert(0, incoming, control)?;
        *checks(&mut self.command)? = replacement.values;
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("view_checks") = replacement.memory;
        }
        Ok(self)
    }

    pub fn record_view_update_inputs(
        mut self,
        relation: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Update(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "view update inputs require UPDATE".into(),
            ));
        };
        let mut assigned = ProductionVec::new(*control);
        for name in plan
            .assignments
            .iter()
            .flat_map(|assignment| assignment.target.column_names())
        {
            assigned.push_produced(control.copy_text(name)?)?;
        }
        let relation = control.copy_text(relation)?;
        let assigned = assigned.finish()?;
        let incoming = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let value = copying.owned_view_update_inputs(relation, assigned)?;
            OwnedFields {
                value,
                memory: Some(copying.into_rewrite_memory()),
            }
        } else {
            OwnedFields {
                value: ViewRuleUpdatePlan {
                    relation: relation
                        .into_uncontrolled()
                        .expect("ordinary rule relation"),
                    assigned_columns: assigned.into_uncontrolled().expect("ordinary rule columns"),
                    input_columns: Vec::new(),
                },
                memory: None,
            }
        };
        let index = plan.view_rule_update_plans.len();
        let values = std::mem::take(&mut plan.view_rule_update_plans);
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| body(memory).field_mut("view_rule_update_plans").take());
        let replacement = FieldVector { values, memory }.insert(index, incoming, control)?;
        plan.view_rule_update_plans = replacement.values;
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("view_rule_update_plans") = replacement.memory;
        }
        Ok(self)
    }

    pub fn finalize_view_update_inputs(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Update(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "view update inputs require UPDATE".into(),
            ));
        };
        for (index, input) in plan.view_rule_update_plans.iter_mut().enumerate() {
            let mut columns = ProductionVec::new(*control);
            for name in plan
                .assignments
                .iter()
                .flat_map(|assignment| assignment.target.column_names())
            {
                columns.push_produced(control.copy_text(name)?)?;
            }
            let (columns, memory) = columns.finish()?.into_parts();
            input.input_columns = columns;
            if let Some(owner) = &mut self.memory {
                *body(owner)
                    .field_mut("view_rule_update_plans")
                    .as_mut()
                    .expect("view rule updates")
                    .item_mut(index)
                    .as_mut()
                    .expect("view rule update")
                    .field_mut("input_columns") = memory.map(|direct| RewriteMemory::Fields {
                    fields: BudgetedVec::new(direct.budget()),
                    direct,
                });
            }
        }
        Ok(self)
    }
    pub fn record_view_insert_inputs(
        mut self,
        relation: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "view insert inputs require INSERT".into(),
            ));
        };
        let mut assigned = ProductionVec::new(*control);
        for name in plan.columns.iter().map(|target| target.column.as_str()) {
            assigned.push_produced(control.copy_text(name)?)?;
        }
        let relation = control.copy_text(relation)?;
        let assigned = assigned.finish()?;
        let incoming = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let value = copying.owned_view_insert_inputs(relation, assigned)?;
            OwnedFields {
                value,
                memory: Some(copying.into_rewrite_memory()),
            }
        } else {
            OwnedFields {
                value: ViewRuleInsertPlan {
                    relation: relation
                        .into_uncontrolled()
                        .expect("ordinary rule relation"),
                    supplied_columns: assigned.into_uncontrolled().expect("ordinary rule columns"),
                    input_columns: Vec::new(),
                },
                memory: None,
            }
        };
        let index = plan.view_rule_insert_plans.len();
        let values = std::mem::take(&mut plan.view_rule_insert_plans);
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| body(memory).field_mut("view_rule_insert_plans").take());
        let replacement = FieldVector { values, memory }.insert(index, incoming, control)?;
        plan.view_rule_insert_plans = replacement.values;
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("view_rule_insert_plans") = replacement.memory;
        }
        Ok(self)
    }

    pub fn finalize_view_insert_inputs(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "view insert inputs require INSERT".into(),
            ));
        };
        for (index, input) in plan.view_rule_insert_plans.iter_mut().enumerate() {
            let mut columns = ProductionVec::new(*control);
            for name in plan.columns.iter().map(|target| target.column.as_str()) {
                columns.push_produced(control.copy_text(name)?)?;
            }
            let (columns, memory) = columns.finish()?.into_parts();
            input.input_columns = columns;
            if let Some(owner) = &mut self.memory {
                *body(owner)
                    .field_mut("view_rule_insert_plans")
                    .as_mut()
                    .expect("view rule inserts")
                    .item_mut(index)
                    .as_mut()
                    .expect("view rule insert")
                    .field_mut("input_columns") = memory.map(|direct| RewriteMemory::Fields {
                    fields: BudgetedVec::new(direct.budget()),
                    direct,
                });
            }
        }
        Ok(self)
    }
}

fn body(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory.field_mut("value0").as_mut().expect("mutation body")
}

fn checks(command: &mut CommandPlan) -> Result<&mut Vec<ViewCheckPlan>, SQLError> {
    match command {
        CommandPlan::Insert(plan) => Ok(&mut plan.view_checks),
        CommandPlan::Update(plan) => Ok(&mut plan.view_checks),
        CommandPlan::Merge(plan) => Ok(&mut plan.view_checks),
        _ => Err(SQLError::Internal(
            "view checks require INSERT, UPDATE or MERGE".into(),
        )),
    }
}
