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
use std::fmt::Write;
use uqa_core::memory::{ProductionControl, ProductionString};
use uqa_sql::{explain::ExplainSettings, SQLError};

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    settings: &ExplainSettings,
    physical: &ExplainPhysicalPlan,
) -> Result<(), SQLError> {
    let Some(planning) = physical.planning else {
        return Ok(());
    };
    let memory = planning.memory.filter(|_| settings.memory);
    let buffers = settings.buffers && buffers::has_usage(planning.io.temporary);
    if buffers || memory.is_some() {
        text.push_str("\nPlanning:")?;
    }
    if buffers {
        buffers::append_text(text, planning.io.temporary, "  ")?;
    }
    if let Some(memory) = memory {
        let _ = write!(
            text,
            "\n  Memory: used={}kB  allocated={}kB",
            memory.used_bytes.div_ceil(1024),
            memory.allocated_bytes.div_ceil(1024),
        );
    }
    Ok(())
}

pub(super) fn properties(
    settings: &ExplainSettings,
    physical: &ExplainPhysicalPlan,
    analysis: Option<&ExplainAnalysis>,
    control: ProductionControl<'_>,
) -> Result<Option<Property<'static>>, SQLError> {
    let Some(planning) = physical.planning else {
        return Ok(None);
    };
    let memory = planning.memory.filter(|_| settings.memory);
    if !settings.buffers && memory.is_none() {
        return Ok(None);
    }
    let mut output = Object::new(control);
    if settings.buffers {
        buffers::properties(
            &mut output,
            planning.io.temporary,
            analysis.map_or(planning.io_timing, |analysis| analysis.io_timing),
        )?;
    }
    if let Some(memory) = memory {
        output.insert(
            "Memory Used",
            Property::Unsigned(memory.used_bytes.div_ceil(1024) as u64),
        )?;
        output.insert(
            "Memory Allocated",
            Property::Unsigned(memory.allocated_bytes.div_ceil(1024) as u64),
        )?;
    }
    Ok(Some(output.finish()?))
}

#[cfg(test)]
mod tests;
