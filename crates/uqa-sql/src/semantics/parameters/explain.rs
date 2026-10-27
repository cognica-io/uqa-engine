//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select only planning parameters and admit their displayed values without a complete settings snapshot.

use super::{
    catalog::parameter_definitions,
    definition::{ParameterDefinition, ParameterFlags},
    value::show_setting_with_control,
};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[cfg(test)]
mod tests;

pub type ExplainSettings = Vec<(&'static str, String)>;
pub type ParameterRead<'a> = dyn Fn(
        &'static ParameterDefinition,
        &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError>
    + 'a;

pub fn capture(
    read: &ParameterRead<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ExplainSettings>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for definition in parameter_definitions()
        .iter()
        .filter(|definition| definition.has_flag(ParameterFlags::EXPLAIN))
    {
        control.check()?;
        let Some(setting) = read(definition, control)? else {
            continue;
        };
        if setting.as_str() == definition.boot_setting_with_control(control)?.as_str() {
            continue;
        }
        let shown = show_setting_with_control(definition, &setting, control)?;
        let (shown, memory) = shown.into_parts();
        output.push_produced(control.finish((definition.name, shown), memory)?)?;
    }
    Ok(output.finish()?)
}
