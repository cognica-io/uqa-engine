//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render measured output conversion without running result expressions.

use super::{
    properties::{Object, Property},
    ExplainAnalysis,
};
use std::fmt::Write as _;
use uqa_core::memory::{ProductionControl, ProductionString};
use uqa_sql::{
    explain::{ExplainSerialization, ExplainSettings},
    SQLError,
};

fn format(settings: &ExplainSettings) -> Option<&'static str> {
    match settings.serialize {
        ExplainSerialization::None => None,
        ExplainSerialization::Text => Some("text"),
        ExplainSerialization::Binary => Some("binary"),
    }
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    settings: &ExplainSettings,
    analysis: &ExplainAnalysis,
) -> Result<(), SQLError> {
    let Some(format) = format(settings) else {
        return Ok(());
    };
    text.push_str("\nSerialization: ")?;
    if settings.timing {
        let _ = write!(
            text,
            "time={:.3} ms  ",
            analysis.serialization.elapsed.as_secs_f64() * 1000.0
        );
    }
    let _ = write!(
        text,
        "output={}kB  format={format}",
        analysis.serialization.bytes.div_ceil(1024)
    );
    Ok(())
}

pub(super) fn properties(
    settings: &ExplainSettings,
    analysis: &ExplainAnalysis,
    control: ProductionControl<'_>,
) -> Result<Option<Property<'static>>, SQLError> {
    let Some(format) = format(settings) else {
        return Ok(None);
    };
    let mut output = Object::new(control);
    output.insert("Format", Property::Text(format))?;
    output.insert(
        "Output Volume",
        Property::Unsigned(analysis.serialization.bytes.div_ceil(1024)),
    )?;
    if settings.timing {
        output.insert(
            "Time",
            Property::Float(analysis.serialization.elapsed.as_secs_f64() * 1000.0),
        )?;
    }
    Ok(Some(output.finish()?))
}
