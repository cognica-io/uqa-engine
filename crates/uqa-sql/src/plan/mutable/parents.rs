//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only parent observations remain available while a child owns its moved payload.

use super::{
    edits::Slot, sources::query_source_memory, MutableQueryPlan, MutableSourcePlan,
    OwnedScalarExpr, QueryChildSlot, QueryPlan, QueryScalarSlot, RelationalPlan,
};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl MutableQueryPlan {
    /// Rewrite a scalar with a read-only view of the enclosing query. The selected parent slot temporarily contains an allocation-free NULL; the callback receives its actual previous scalar and owner separately.
    pub fn map_scalar_with_parent(
        mut self,
        slot: QueryScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut position) = self.scalar_slot(slot) else {
            return Ok(self);
        };
        let input = position.take();
        let output = input.rewrite(control, |input| map(&self.query, input))?;
        self.scalar_slot(slot)
            .expect("original scalar slot")
            .restore(output);
        Ok(self)
    }

    /// Observe an enclosing query's namespace while rewriting one directly owned child. Its original box or arena entry stays allocated in the parent and holds an empty query until the callback returns.
    pub fn map_query_child_with_parent(
        mut self,
        slot: QueryChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = self.query_slot(slot).take();
        let output = map(&self.query, input)?;
        output.assert_owner(control);
        control.check()?;
        self.query_slot(slot).restore(output);
        Ok(self)
    }

    /// Observe the query's scalar and analysis inputs while rewriting its source. FROM is temporarily absent; its complete original source is moved into the callback.
    pub fn map_source_with_parent(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&QueryPlan, MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut slot) = self.source_slot()? else {
            return Ok(self);
        };
        let input = slot.take();
        let output = map(&self.query, input)?;
        output.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            unreachable!("read-only parent cannot change topology");
        };
        Slot::<MutableSourcePlan>::optional(
            &mut block.from,
            self.memory.as_mut().map(query_source_memory),
        )
        .restore(output);
        Ok(self)
    }
}
