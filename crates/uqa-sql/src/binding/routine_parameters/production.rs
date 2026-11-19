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
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryBudget, Produced, ProductionControl, ProductionVec},
    CancellationToken,
};

#[cfg(test)]
mod input_tests;
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
        Self::publish(schema, function, names.finish()?, control)
    }

    /// Build a complete parameter namespace through retained work. The selected decoded names and types remain borrowed until its immutable generation is published.
    pub async fn new_from_input(
        function: &str,
        columns: &[(&str, Option<&ColumnType>)],
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        let schema = RowSchema::with_parameter_metadata_from_input(
            function,
            *ROUTINE_PARAMETERS,
            columns,
            memory,
            original,
            input,
        )
        .await?;
        let function = crate::catalog::work::Work::text(input, function, &control).await?;
        let mut names = ProductionVec::new(control);
        names.reserve(columns.len())?;
        for (name, _) in columns {
            control.check()?;
            input.checkpoint().await?;
            control.check()?;
            names.push_produced(crate::catalog::work::Work::text(input, name, &control).await?)?;
        }
        input.checkpoint().await?;
        control.check()?;
        Self::publish(schema, function, names.finish()?, &control)
    }

    fn publish(
        schema: RowSchema,
        function: Produced<String>,
        names: Produced<Vec<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (function, function_memory) = function.into_parts();
        let (names, names_memory) = names.into_parts();
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
