//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render the captured planning observation independently of execution and summary timing.

use super::{
    buffers,
    properties::{Object, Property},
    ExplainAnalysis, ExplainPhysicalPlan,
};
use uqa_core::memory::{ProductionControl, ProductionString};
use uqa_sql::{explain::ExplainSettings, SQLError};

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    settings: &ExplainSettings,
    physical: &ExplainPhysicalPlan,
) -> Result<(), SQLError> {
    let Some(planning) = physical.planning.filter(|_| settings.buffers) else {
        return Ok(());
    };
    if buffers::has_usage(planning.io.temporary) {
        text.push_str("\nPlanning:")?;
        buffers::append_text(text, planning.io.temporary, "  ")?;
    }
    Ok(())
}

pub(super) fn properties(
    settings: &ExplainSettings,
    physical: &ExplainPhysicalPlan,
    analysis: Option<&ExplainAnalysis>,
    control: ProductionControl<'_>,
) -> Result<Option<Property<'static>>, SQLError> {
    let Some(planning) = physical.planning.filter(|_| settings.buffers) else {
        return Ok(None);
    };
    let mut output = Object::new(control);
    buffers::properties(
        &mut output,
        planning.io.temporary,
        analysis.map_or(planning.io_timing, |analysis| analysis.io_timing),
    )?;
    Ok(Some(output.finish()?))
}

#[cfg(test)]
mod tests;
