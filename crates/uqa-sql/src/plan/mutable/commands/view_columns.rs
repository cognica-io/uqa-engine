//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View column translation replaces only the selected names; indirection and values keep their owners.

use super::{item, CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::plan::{copy::Copying, MergeWhenPlan};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl MutableCommandPlan {
    pub fn map_insert_column(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(&str) -> Result<Produced<String>, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "INSERT column translation requires INSERT".into(),
            ));
        };
        let memory = self
            .memory
            .as_mut()
            .map(|memory| item(body(memory), "columns", index));
        super::assignments::rename_target(&mut plan.columns[index], memory, control, &mut map)?;
        Ok(self)
    }

    pub fn map_merge_insert_column(
        mut self,
        clause: usize,
        index: usize,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(&str) -> Result<Produced<String>, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Merge(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "MERGE column translation requires MERGE".into(),
            ));
        };
        let MergeWhenPlan::InsertNotMatched { columns, .. } = &mut plan.when_clauses[clause] else {
            return Err(SQLError::Internal(
                "MERGE column translation requires an INSERT action".into(),
            ));
        };
        let memory = self
            .memory
            .as_mut()
            .map(|memory| item(item(body(memory), "when_clauses", clause), "columns", index));
        super::assignments::rename_target(&mut columns[index], memory, control, &mut map)?;
        Ok(self)
    }

    pub fn set_implicit_merge_columns<'a>(
        mut self,
        clause: usize,
        names: impl ExactSizeIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Merge(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "implicit MERGE columns require MERGE".into(),
            ));
        };
        let MergeWhenPlan::InsertNotMatched { columns, .. } = &mut plan.when_clauses[clause] else {
            return Err(SQLError::Internal(
                "implicit MERGE columns require an INSERT action".into(),
            ));
        };
        if !columns.is_empty() {
            return Ok(self);
        }
        let (output, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let columns = copying.implicit_insert_columns(names)?;
            (columns, Some(copying.into_rewrite_memory()))
        } else {
            (
                names
                    .map(|name| crate::ast::AssignmentTarget::from(name.to_owned()))
                    .collect(),
                None,
            )
        };
        *columns = output;
        if let Some(owner) = &mut self.memory {
            *item(body(owner), "when_clauses", clause).field_mut("columns") = memory;
        }
        Ok(self)
    }

    pub fn map_conflict_columns(
        mut self,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(&str) -> Result<Produced<String>, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal("conflict columns require INSERT".into()));
        };
        let Some(conflict) = &mut plan.on_conflict else {
            return Ok(self);
        };
        for (index, name) in conflict.conflict_columns.iter_mut().enumerate() {
            let (output, memory) = map(name)?.into_parts();
            let (output, memory) = control.finish(output, memory)?.into_parts();
            *name = output;
            if let Some(owner) = &mut self.memory {
                *body(owner)
                    .field_mut("on_conflict")
                    .as_mut()
                    .expect("conflict")
                    .field_mut("conflict_columns")
                    .as_mut()
                    .expect("conflict columns")
                    .item_mut(index) = memory.map(|direct| RewriteMemory::Fields {
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
