//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query and mutation CTE bodies move independently of their declaration metadata.

use super::{memory::RewriteMemory, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan};
use crate::{
    plan::{CommandPlan, CtePlanBody},
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl MutableQueryPlan {
    /// Rewrite one CTE body while preserving its query/command kind and original declaration box. Its name, columns and scheduling metadata stay with the parent query.
    pub fn map_cte(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let memory = self.memory.as_mut().map(|memory| cte_memory(memory, index));
        rewrite_body(&mut self.query.ctes[index].body, memory, control, map)?;
        Ok(self)
    }
}

impl MutableCommandPlan {
    /// Rewrite a data-modifying command's CTE without copying its declaration or enclosing box.
    pub fn map_cte(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let memory = self.memory.as_mut().map(|memory| {
            cte_memory(
                memory.field_mut("value0").as_mut().expect("mutation body"),
                index,
            )
        });
        let ctes = self.command.ctes_mut().expect("command owns CTEs");
        rewrite_body(&mut ctes[index].body, memory, control, map)?;
        Ok(self)
    }
}

fn cte_memory(memory: &mut RewriteMemory, index: usize) -> &mut RewriteMemory {
    memory
        .field_mut("ctes")
        .as_mut()
        .expect("CTE vector")
        .item_mut(index)
        .as_mut()
        .expect("CTE entry")
        .field_mut("body")
        .as_mut()
        .expect("CTE body box")
}

fn rewrite_body(
    body: &mut CtePlanBody,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    map: impl FnOnce(MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError>,
) -> Result<(), SQLError> {
    match body {
        CtePlanBody::Query(query) => {
            let input = MutableQueryPlan {
                query: std::mem::replace(query.as_mut(), super::queries::empty_query()),
                memory: memory
                    .as_mut()
                    .and_then(|memory| memory.field_mut("query").take()),
            };
            let MutableExecutablePlan::Query(output) = map(MutableExecutablePlan::Query(input))?
            else {
                return Err(SQLError::Internal(
                    "CTE rewrite changed a query body into a command".into(),
                ));
            };
            output.assert_owner(control);
            control.check()?;
            **query = output.query;
            if let Some(memory) = memory {
                *memory.field_mut("query") = output.memory;
            }
        }
        CtePlanBody::Command(command) => {
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
                    "CTE rewrite changed a command body into a query".into(),
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
