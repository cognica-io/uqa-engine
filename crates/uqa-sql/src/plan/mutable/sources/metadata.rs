//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{MutableSourcePlan, SourcePlan};
use crate::{plan::JoinExecutionStrategy, SQLError};
use uqa_core::memory::ProductionControl;

impl MutableSourcePlan {
    /// Retain the selected join's lateral namespace when a function input expands into a derived table.
    pub fn set_join_lateral(
        mut self,
        lateral: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let SourcePlan::Join {
            lateral: target, ..
        } = &mut self.source
        else {
            return Err(SQLError::Internal(
                "lateral join decision requires a join source".into(),
            ));
        };
        *target = lateral;
        Ok(self)
    }

    /// Install the planner's allocation-free join execution decision.
    pub fn set_join_strategy(
        mut self,
        strategy: JoinExecutionStrategy,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let SourcePlan::Join {
            strategy: target, ..
        } = &mut self.source
        else {
            return Err(SQLError::Internal(
                "join strategy requires a join source".into(),
            ));
        };
        *target = strategy;
        Ok(self)
    }
}
