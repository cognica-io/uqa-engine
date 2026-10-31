//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::{cte::rows::CteRows, CteScope};
use uqa_core::memory::ProductionControl;
use uqa_sql::{plan::CtePlan, SQLError};

impl<S: Clone> CteScope<S> {
    pub fn materialized_for_scan_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<CteRows>, SQLError> {
        control.check()?;
        let rows = self.materialized_for_scan(reference);
        control.check()?;
        Ok(rows)
    }
    pub fn deferred_reference_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&CtePlan>, SQLError> {
        control.check()?;
        let plan = self.deferred_reference(reference);
        control.check()?;
        Ok(plan)
    }
    pub fn is_visible_cte_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let visible = self.is_visible_cte(reference);
        control.check()?;
        Ok(visible)
    }
}
