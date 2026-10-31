//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    binding::{
        production::{ControlledBindingContext, ControlledBindingSnapshot},
        snapshot::BindingSnapshot,
    },
    plan::QueryPlan,
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub trait BindingTypeScope: Sync {
    fn scalar_subqueries(&self) -> &[QueryPlan];
    fn type_context(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'_>, SQLError>;
}

impl BindingTypeScope for ControlledBindingContext<'_> {
    fn scalar_subqueries(&self) -> &[QueryPlan] {
        self.scalar_subqueries
    }
    fn type_context(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'_>, SQLError> {
        self.copy_with_control(control)
    }
}

impl BindingTypeScope for ControlledBindingSnapshot {
    fn scalar_subqueries(&self) -> &[QueryPlan] {
        &self.scalar_subqueries
    }
    fn type_context(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'_>, SQLError> {
        self.context_with_control(control)
    }
}
impl BindingTypeScope for BindingSnapshot {
    fn scalar_subqueries(&self) -> &[QueryPlan] {
        &self.scalar_subqueries
    }
    fn type_context(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'_>, SQLError> {
        let copied = self.copy_with_control(control)?;
        Ok(ControlledBindingContext {
            catalog: copied.catalog,
            resolution: copied.resolution,
            ctes: copied.ctes,
            deferred_ctes: copied.deferred_ctes,
            non_returning_ctes: copied.non_returning_ctes,
            scalar_subqueries: &self.scalar_subqueries,
        })
    }
}
