//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host type input shares catalog resolution and native SQL coercion.

use crate::{
    expr::EngineHook, type_resolution::FunctionTypeResolver, ColumnType, SQLError, SQLParam,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

#[cfg(test)]
mod tests;

/// Resolve a type declaration and read one external text or SQL NULL value. The caller owns the statement/catalog scope; domain constraints and other input callbacks execute within that scope.
pub fn create_typed_parameter_with_control(
    type_name: &str,
    text: Option<&str>,
    types: &dyn FunctionTypeResolver,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    control.check()?;
    let ty = resolve_type(type_name, types, control)?;
    // Reading a value has CAST semantics. Type USAGE governs declarations, not value input.
    let value = match text {
        Some(text) => {
            let (text, memory) = control.copy_text(text)?.into_parts();
            control.finish(Value::Str(text), memory)?
        }
        None => control.finish(Value::Null, control.empty_reservation())?,
    };
    // Catalog identities keep the selected type stable through input callbacks and search-path changes.
    let name = ty.catalog_name_with_control(control)?;
    let value = crate::expr::cast_value_with_type_resolution_with_control(
        &value,
        Some("unknown"),
        &name,
        Some(input),
        control,
    )?;
    finish(value, ty, control)
}

pub(super) fn bind_sources(
    parameters: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let mut sources = ProductionVec::new(*control);
    sources.reserve(parameters.len())?;
    for parameter in parameters {
        let source = super::bind_parameter_type_with_control(parameter, types, control)?;
        let (source, memory) = match source {
            Some(source) => {
                let (ty, memory) = source.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        sources.push_produced(control.finish(source, memory)?)?;
    }
    Ok(sources.finish()?)
}

pub(super) fn cast_parameter(
    parameter: &SQLParam,
    source: Option<&ColumnType>,
    target: &ColumnType,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let source = source
        .map(|ty| ty.catalog_name_with_control(control))
        .transpose()?;
    let target = target.catalog_name_with_control(control)?;
    let owned;
    let value = if let Some(value) = parameter.scalar_value() {
        value
    } else {
        owned = parameter.to_value_with_control(control)?;
        &owned
    };
    crate::expr::cast_value_with_type_resolution_with_control(
        value,
        Some(source.as_deref().map_or("unknown", String::as_str)),
        &target,
        Some(input),
        control,
    )
}

pub(super) fn resolve_type(
    type_name: &str,
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    Ok(match types.resolve_type_name(type_name)? {
        Some(ty) => ty.retain_external_with_control(control)?,
        None => ColumnType::from_sql_name_with_control(type_name, control)?,
    })
}

pub(super) fn finish(
    value: Produced<Value>,
    ty: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let (value, value_memory) = value.into_parts();
    let (ty, type_memory) = ty.into_parts();
    Ok(control.finish(
        SQLParam::typed_scalar(value, ty),
        control.combine(value_memory, type_memory),
    )?)
}
