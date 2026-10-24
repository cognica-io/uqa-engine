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
    memory::{Produced, ProductionControl},
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
    let ty = match types.resolve_type_name(type_name)? {
        Some(ty) => ty.retain_external_with_control(control)?,
        None => ColumnType::from_sql_name_with_control(type_name, control)?,
    };
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
    let (value, value_memory) = value.into_parts();
    let (ty, type_memory) = ty.into_parts();
    Ok(control.finish(
        SQLParam::typed_scalar(value, ty),
        control.combine(value_memory, type_memory),
    )?)
}
