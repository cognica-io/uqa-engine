//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::{cte::rows::CteRows, CteScope};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{plan::CtePlan, SQLError};

fn name(
    reference: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    control.check()?;
    let Some(reference) = uqa_core::RelationIdentity::borrowed_reference(reference) else {
        return Ok(None);
    };
    if reference.schema.is_some() {
        return Ok(None);
    }
    Ok(Some(control.format(format_args!("{}", reference.name))?))
}

impl<S: Clone> CteScope<S> {
    pub fn materialized_for_scan_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<CteRows>, SQLError> {
        Ok(name(reference, control)?.and_then(|name| self.rows.get(&name).cloned()))
    }
    pub fn deferred_reference_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&CtePlan>, SQLError> {
        Ok(name(reference, control)?
            .and_then(|name| self.deferred_ctes.get(name.as_str()).map(AsRef::as_ref)))
    }
    pub fn is_visible_cte_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        Ok(name(reference, control)?.is_some_and(|name| self.contains_cte_name(&name)))
    }
}
