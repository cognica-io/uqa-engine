//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{MutableQueryPlan, MutableSourcePlan, SQLError, SourceChildSlot, SourcePlan};
use uqa_core::memory::ProductionControl;

impl MutableSourcePlan {
    /// Observe source aliases and retained descriptors while rewriting the source's query. The original box stays in the source with an empty placeholder during the callback.
    pub fn map_query_with_parent(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&SourcePlan, MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut slot) = self.query_slot()? else {
            return Ok(self);
        };
        let input = slot.take();
        let output = map(&self.source, input)?;
        output.assert_owner(control);
        control.check()?;
        self.query_slot()?
            .expect("original source query")
            .restore(output);
        Ok(self)
    }

    /// Observe the join's metadata and other input while moving one source child through a rewrite.
    pub fn map_child_with_parent(
        mut self,
        slot: SourceChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&SourcePlan, Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = self.child_slot(slot).take();
        let output = map(&self.source, input)?;
        output.assert_owner(control);
        control.check()?;
        self.child_slot(slot).restore(output);
        Ok(self)
    }
}
