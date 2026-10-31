//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected function result metadata admits final column names before producing their buffers.

use super::{
    column_definitions::ResultClass, ColumnType, FunctionReturns, RoutineInvocationBinding,
    RoutineResolution, SQLError, SQLUserFunction,
};
use crate::expr::composites::{descriptor_with_control, OwnedCompositeTypeDescriptor};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn composite_result_oid_with_control(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<Option<u32>, SQLError> {
    control.check()?;
    let mut outputs = function.def.output_params();
    if outputs.clone().nth(1).is_some() {
        return Ok(None);
    }
    let declared = match (&function.def.returns, outputs.next()) {
        (_, Some(output)) => Some(output.type_name.as_str()),
        (FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }, None) => {
            Some(type_name.as_str())
        }
        _ => None,
    };
    let Some(name) = invocation
        .and_then(|value| value.return_type.as_deref())
        .or(declared)
    else {
        return Ok(None);
    };
    if crate::type_resolution::routine_polymorphic_type_with_control(name, control)?.is_some() {
        return Ok(None);
    }
    let ty = routines.resolve_type_name_with_control(name, control)?;
    Ok(match ty.as_deref() {
        Some(ColumnType::Composite(reference)) => Some(reference.oid),
        _ => None,
    })
}

pub fn user_function_composite_result_with_control(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
    composite_result_oid_with_control(routines, function, invocation, control)?
        .map(|oid| descriptor_with_control(routines.composite_types(), oid, control))
        .transpose()
}

pub fn user_function_output_columns_with_control(
    function: &SQLUserFunction,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    output_columns(function, &[], false, control)
}

pub fn apply_table_function_aliases_with_control(
    names: &[String],
    aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    columns(
        names.iter().map(|name| ColumnName::Written(name)),
        names.len(),
        aliases,
        ordinality,
        control,
    )
}

pub fn user_function_declared_columns_with_control(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    declared_columns(routines, function, invocation, &[], false, control)
}

#[expect(
    clippy::too_many_arguments,
    reason = "selected source metadata and its caller-owned admission scope remain independently borrowed"
)]
pub fn user_table_function_columns_with_control(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    output_name: &str,
    alias: Option<&str>,
    aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let columns =
        if user_function_result_class_with_control(routines, function, invocation, control)?
            == ResultClass::Record
        {
            columns(
                aliases.iter().map(|name| ColumnName::Written(name)),
                aliases.len(),
                aliases,
                ordinality,
                control,
            )?
        } else if let Some(columns) =
            declared_columns(routines, function, invocation, aliases, ordinality, control)?
        {
            columns
        } else {
            columns(
                std::iter::once(ColumnName::Written(alias.unwrap_or(output_name))),
                1,
                aliases,
                ordinality,
                control,
            )?
        };
    validate_table_function_alias_count_with_control(
        alias.unwrap_or(output_name),
        columns.len(),
        aliases.len(),
        control,
    )?;
    Ok(columns)
}

fn declared_columns(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    match user_function_composite_result_with_control(routines, function, invocation, control)? {
        Some(descriptor) => columns(
            descriptor
                .attributes
                .iter()
                .map(|attribute| ColumnName::Written(&attribute.name)),
            descriptor.attributes.len(),
            aliases,
            ordinality,
            control,
        )
        .map(Some),
        None => output_columns(function, aliases, ordinality, control),
    }
}

fn output_columns(
    function: &SQLUserFunction,
    aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    control.check()?;
    let outputs = function.def.output_params();
    if outputs.clone().next().is_none() {
        return Ok(None);
    }
    let count = outputs.clone().count();
    columns(
        outputs.enumerate().map(|(index, parameter)| {
            if parameter.name.is_empty() {
                ColumnName::Ordinal(index + 1)
            } else {
                ColumnName::Written(&parameter.name)
            }
        }),
        count,
        aliases,
        ordinality,
        control,
    )
    .map(Some)
}

enum ColumnName<'a> {
    Written(&'a str),
    Ordinal(usize),
}

fn columns<'a>(
    names: impl Iterator<Item = ColumnName<'a>>,
    count: usize,
    aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(count + usize::from(ordinality))?;
    for (index, name) in names.enumerate() {
        control.check()?;
        output.push_produced(match aliases.get(index) {
            Some(alias) => control.copy_text(alias)?,
            None => match name {
                ColumnName::Written(name) => control.copy_text(name)?,
                ColumnName::Ordinal(index) => control.format(format_args!("column{index}"))?,
            },
        })?;
    }
    if ordinality {
        output.push_produced(
            control.copy_text(aliases.get(count).map_or("ordinality", String::as_str))?,
        )?;
    }
    Ok(output.finish()?)
}

pub(super) fn user_function_result_class_with_control(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
    control: &ProductionControl<'_>,
) -> Result<ResultClass, SQLError> {
    control.check()?;
    if crate::routines::routine_returns_anonymous_record_with_control(&function.def, control)? {
        return Ok(ResultClass::Record);
    }
    if function.def.output_params().nth(1).is_some() {
        return Ok(ResultClass::OutParameters);
    }
    let output = function
        .def
        .params
        .iter()
        .enumerate()
        .find(|(_, parameter)| {
            matches!(
                parameter.mode,
                crate::ast::FunctionParamMode::Out
                    | crate::ast::FunctionParamMode::InOut
                    | crate::ast::FunctionParamMode::Table
            )
        });
    let (declared, concrete) = match (output, &function.def.returns) {
        (Some((position, output)), _) => (
            output.type_name.as_str(),
            invocation
                .and_then(|invocation| invocation.parameter_types.get(position))
                .map(String::as_str),
        ),
        (None, FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) => (
            type_name.as_str(),
            invocation.and_then(|invocation| invocation.return_type.as_deref()),
        ),
        _ => return Ok(ResultClass::Scalar),
    };
    let name = concrete.unwrap_or(declared);
    if crate::type_resolution::routine_polymorphic_type_with_control(name, control)?.is_some() {
        return Ok(ResultClass::Unclassified);
    }
    Ok(routines
        .resolve_type_name_with_control(name, control)?
        .as_deref()
        .map_or(
            ResultClass::Scalar,
            super::column_definitions::type_result_class,
        ))
}

pub fn validate_table_function_alias_count_with_control(
    table_alias: &str,
    available: usize,
    specified: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if specified <= available {
        return Ok(());
    }
    let message = control.format(format_args!("table \"{table_alias}\" has {available} columns available but {specified} columns specified"))?;
    let sqlstate = control.copy_text("42P10")?;
    Err(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}
