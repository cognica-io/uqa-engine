//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query and mutation CTE bodies move independently of their declaration metadata.

use super::{
    edits::{BodySlot, Slot},
    memory::RewriteMemory,
    MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
};
use crate::{plan::CtePlanBody, SQLError};
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
        let mut slot = self.cte_slot(index);
        let output = map(slot.take())?;
        slot.complete(output, control, "CTE")?;
        Ok(self)
    }

    pub(super) fn cte_slot(&mut self, index: usize) -> BodySlot<'_> {
        let memory = self.memory.as_mut().map(|memory| cte_memory(memory, index));
        body_slot(&mut self.query.ctes[index].body, memory)
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
        let mut slot = self.cte_slot(index);
        let output = map(slot.take())?;
        slot.complete(output, control, "CTE")?;
        Ok(self)
    }

    pub(super) fn cte_slot(&mut self, index: usize) -> BodySlot<'_> {
        let memory = self.memory.as_mut().map(|memory| {
            cte_memory(
                memory.field_mut("value0").as_mut().expect("mutation body"),
                index,
            )
        });
        let ctes = self.command.ctes_mut().expect("command owns CTEs");
        body_slot(&mut ctes[index].body, memory)
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

fn body_slot<'a>(body: &'a mut CtePlanBody, memory: Option<&'a mut RewriteMemory>) -> BodySlot<'a> {
    match body {
        CtePlanBody::Query(query) => BodySlot::Query(Slot::new(
            query.as_mut(),
            memory.map(|memory| memory.field_mut("query")),
        )),
        CtePlanBody::Command(command) => BodySlot::Command(Slot::new(
            command.as_mut(),
            memory.map(|memory| memory.field_mut("command")),
        )),
    }
}
