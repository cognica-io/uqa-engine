//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A routine namespace shares its immutable admitted names and schema across analysis scopes.

use super::{
    ColumnType, RoutineParameterData, RoutineParameterScope, RowSchema, SQLError,
    ROUTINE_PARAMETERS,
};
use uqa_core::memory::{ProductionControl, ProductionVec};

#[cfg(test)]
mod tests;

impl RoutineParameterScope {
    pub fn new_with_control<'a>(
        function: &str,
        columns: impl Iterator<Item = (&'a str, Option<&'a ColumnType>)> + Clone,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let schema = RowSchema::with_parameter_metadata_with_control(
            function,
            *ROUTINE_PARAMETERS,
            columns.clone(),
            control,
        )?;
        let function = control.copy_text(function)?;
        let mut names = ProductionVec::new(*control);
        names.reserve(columns.clone().count())?;
        for (name, _) in columns {
            names.push_produced(control.copy_text(name)?)?;
        }
        let (function, function_memory) = function.into_parts();
        let (names, names_memory) = names.finish()?.into_parts();
        let data = control
            .finish(
                RoutineParameterData {
                    function,
                    names,
                    schema,
                },
                control.combine(function_memory, names_memory),
            )?
            .into_shared(control)?;
        Ok(Self { data })
    }
}
