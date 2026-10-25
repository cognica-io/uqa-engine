//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host record construction shares positional SQL casts and the native row producer.

use super::{input, SQLParam};
use crate::{expr::EngineHook, ColumnType, FunctionTypeResolver, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[cfg(test)]
mod tests;

/// Bind every source before converting fields in live attribute order. The caller supplies the statement scope for input callbacks and enclosing domain constraints.
pub fn create_record_parameter_with_control(
    type_name: &str,
    fields: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let ty = input::resolve_type(type_name, types, control)?;
    let mut base = &*ty;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if matches!(base, ColumnType::Record) {
        let sources = input::bind_sources(fields, types, control)?;
        return input::finish(anonymous(fields, &sources, control)?, ty, control);
    }
    let ColumnType::Composite(reference) = base else {
        return Err(SQLError::TypeMismatch(
            "record construction requires a named composite type".into(),
        ));
    };
    let descriptor = crate::expr::composites::descriptor(input.composite_types(), reference.oid)?;
    let sources = input::bind_sources(fields, types, control)?;
    if fields.len() != descriptor.attributes.len() {
        return Err(SQLError::Diagnostic {
            sqlstate: "42846".into(),
            message: format!("cannot cast type record to {}", base.display_name()),
            detail: Some(
                if fields.len() < descriptor.attributes.len() {
                    "Input has too few columns."
                } else {
                    "Input has too many columns."
                }
                .into(),
            ),
            hint: None,
        });
    }
    let value = crate::expr::composites::constructor::construct_current_with_control(
        &descriptor,
        control,
        |index| {
            input::cast_parameter(
                &fields[index],
                sources[index].as_ref(),
                &descriptor.attributes[index].ty,
                input,
                control,
            )
        },
    )?;
    let value = if matches!(*ty, ColumnType::Domain { .. }) {
        let source = base.catalog_name_with_control(control)?;
        let target = ty.catalog_name_with_control(control)?;
        crate::expr::cast_value_with_type_resolution_with_control(
            &value,
            Some(&source),
            &target,
            Some(input),
            control,
        )?
    } else {
        value
    };
    input::finish(value, ty, control)
}

fn anonymous(
    fields: &[SQLParam],
    sources: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<uqa_core::Value>, SQLError> {
    use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
    let mut values = ProductionVec::new(*control);
    let mut metadata = ProductionVec::new(*control);
    values.reserve(fields.len())?;
    metadata.reserve(fields.len())?;
    for (field, source) in fields.iter().zip(sources) {
        values.push_produced(field.to_value_with_control(control)?)?;
        metadata.push_copy(uqa_core::RecordFieldType {
            oid: source.as_ref().map_or(705, |ty| pg_type_oid(ty) as u32),
            type_modifier: source.as_ref().map_or(-1, |ty| pg_type_modifier(ty) as i32),
            collation_oid: Some(
                source
                    .as_ref()
                    .map_or(0, |ty| pg_type_collation_oid(ty) as u32),
            ),
        })?;
    }
    let (row, memory) =
        uqa_core::RowValue::typed_with_control(values.finish()?, metadata.finish()?, control)?
            .into_parts();
    Ok(control.finish(uqa_core::Value::Row(row), memory)?)
}
