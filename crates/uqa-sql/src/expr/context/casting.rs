//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware cast selection shares controlled type, array and value constructors.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    ArrayValue, Value,
};

use super::super::casting::{cast_value_from_with_control, parse_pg_array_literal_with_control};
use super::{format_regtype_value_with_control, EngineHook};
use crate::{
    ast::ColumnType,
    error::{Result, SQLError},
};

#[must_use]
pub fn coercion_type_name(ty: &ColumnType) -> String {
    coercion_type_name_with_control(ty, &ProductionControl::uncontrolled())
        .expect("ordinary coercion type name")
        .into_uncontrolled()
        .expect("ordinary coercion type name owner")
}

fn coercion_type_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    match ty {
        ColumnType::Domain { base, .. } => coercion_type_name_with_control(base, control),
        ColumnType::Array(element) => {
            let element = coercion_type_name_with_control(element, control)?;
            let mut name = ProductionString::new(*control);
            name.push_str(&element)?;
            name.push_str("[]")?;
            Ok(name.finish()?)
        }
        _ => Ok(ty.sql_name_with_control(control)?),
    }
}

fn regrole_array_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => {
            matches!(element.as_ref(), ColumnType::Regrole) || regrole_array_type(element)
        }
        _ => false,
    }
}

fn array_leaf_type(ty: &ColumnType) -> &ColumnType {
    match ty {
        ColumnType::Array(element) => array_leaf_type(element),
        _ => ty,
    }
}

fn optional_type_name(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>> {
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Cast a value after resolving catalog-owned types and enforcing domain constraints before exposing a base-type carrier.
pub fn cast_value_with_type_resolution(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
) -> Result<Value> {
    cast_value_with_type_resolution_with_control(
        value,
        source_ty,
        target_ty,
        engine,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary catalog cast owner".into()))
}

/// Resolve catalog inputs at their external handoff, then admit SQL-owned names, element conversions and output before constructing them.
pub fn cast_value_with_type_resolution_with_control(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let resolved_target = engine
        .map(|engine| engine.resolve_type_name(target_ty))
        .transpose()
        .map_err(SQLError::Internal)?
        .flatten()
        .map(|ty| ty.retain_external_with_control(control))
        .transpose()?;
    control.check()?;
    if let (Some(engine), Some(target)) = (engine, resolved_target.as_deref()) {
        if let Some(value) = engine.cast_domain(value, source_ty, target)? {
            return Ok(control.retain_external_value(value)?);
        }
        control.check()?;
        if matches!(target, ColumnType::Array(_)) && requires_catalog_array_cast(target) {
            return cast_catalog_array(value, source_ty, target, engine, control);
        }
    }
    let resolved_source = match (engine, source_ty) {
        (Some(engine), Some(source_ty)) => engine
            .resolve_type_name(source_ty)
            .map_err(SQLError::Internal)?
            .map(|ty| {
                let ty = ty.retain_external_with_control(control)?;
                coercion_type_name_with_control(&ty, control)
            })
            .transpose()?,
        _ => None,
    };
    let source_ty = resolved_source
        .as_ref()
        .map(|name| name.as_str())
        .or(source_ty);
    let target_name = resolved_target
        .as_ref()
        .map(|ty| coercion_type_name_with_control(ty, control))
        .transpose()?;
    let target_ty = target_name.as_ref().map_or(target_ty, |name| name.as_str());
    let parsed_target = if resolved_target.is_some() {
        None
    } else {
        optional_type_name(target_ty, control)?
    };
    let target_column_type = resolved_target.as_deref().or(parsed_target.as_deref());
    cast_resolved_value(
        value,
        source_ty,
        target_ty,
        target_column_type,
        engine,
        control,
    )
}

fn cast_resolved_value(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    target_column_type: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if target_column_type.is_some_and(regrole_array_type) {
        let source_column_type = source_ty
            .map(|name| optional_type_name(name, control))
            .transpose()?
            .flatten();
        let source_name = source_column_type
            .as_deref()
            .map(array_leaf_type)
            .map(|ty| ty.sql_name_with_control(control))
            .transpose()?;
        return cast_array(
            value,
            source_name.as_ref().map(|name| name.as_str()),
            "regrole",
            "regrole[]",
            engine,
            control,
        );
    }
    if target_ty.eq_ignore_ascii_case("text") {
        if let Some(source_ty) = source_ty
            .map(|source| optional_type_name(source, control))
            .transpose()?
            .flatten()
        {
            if let Some(text) =
                format_regtype_value_with_control(value, &source_ty, engine, control)?
            {
                let (text, memory) = text.into_parts();
                return Ok(control.finish(Value::Str(text), memory)?);
            }
        }
    }
    if let (Some(engine), Value::Str(name) | Value::FixedChar(name)) = (engine, value) {
        let oid = resolve_regobject_input(name, target_ty, target_column_type, engine, control)?;
        if let Some(oid) = oid {
            return Ok(control.finish(Value::Int(oid), control.empty_reservation())?);
        }
    }
    cast_value_from_with_control(value, target_ty, source_ty, control)
}

fn resolve_regobject_input(
    name: &str,
    target_ty: &str,
    target_column_type: Option<&ColumnType>,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>> {
    enum ObjectKind {
        Relation,
        Routine,
        Role,
        Namespace,
        Type,
    }
    let kind = if target_ty.eq_ignore_ascii_case("regclass") {
        ObjectKind::Relation
    } else if target_ty.eq_ignore_ascii_case("regprocedure") {
        ObjectKind::Routine
    } else if target_ty.eq_ignore_ascii_case("regrole") {
        ObjectKind::Role
    } else if matches!(target_column_type, Some(ColumnType::Regnamespace)) {
        ObjectKind::Namespace
    } else if matches!(target_column_type, Some(ColumnType::Regtype)) {
        ObjectKind::Type
    } else {
        return Ok(None);
    };
    let oid = match kind {
        ObjectKind::Relation => engine.resolve_regclass_input(name)?,
        ObjectKind::Routine => engine
            .resolve_regprocedure(name)
            .map_err(SQLError::Internal)?,
        ObjectKind::Role => engine.resolve_regrole(name)?,
        ObjectKind::Namespace => engine.resolve_regnamespace(name)?,
        ObjectKind::Type => engine.resolve_regtype_input(name)?,
    };
    control.check()?;
    if oid.is_some() || matches!(kind, ObjectKind::Type) {
        return Ok(oid);
    }
    let (sqlstate, message) = match kind {
        ObjectKind::Relation => ("42P01", format!("relation \"{name}\" does not exist")),
        ObjectKind::Routine => ("42883", format!("function {name} does not exist")),
        ObjectKind::Role => ("42704", format!("role \"{name}\" does not exist")),
        ObjectKind::Namespace => ("3F000", format!("schema \"{name}\" does not exist")),
        ObjectKind::Type => unreachable!("unresolved regtype uses ordinary conversion"),
    };
    Err(SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    })
}

fn requires_catalog_array_cast(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { .. } | ColumnType::Regtype => true,
        ColumnType::Array(element) => requires_catalog_array_cast(element),
        _ => false,
    }
}

fn cast_catalog_array(
    value: &Value,
    source: Option<&str>,
    target: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if matches!(value, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let source_element = source.map(|name| name.trim_end_matches("[]"));
    let target_element = array_leaf_type(target).sql_name_with_control(control)?;
    let target_name = target.sql_name_with_control(control)?;
    cast_array(
        value,
        source_element,
        &target_element,
        &target_name,
        Some(engine),
        control,
    )
}

fn cast_array(
    value: &Value,
    source: Option<&str>,
    target_element: &str,
    target_name: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let parsed;
    let array = match value {
        Value::Array(array) => array,
        Value::Str(text) => {
            parsed = parse_pg_array_literal_with_control(text, control)?;
            &parsed
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "CAST AS {target_name}: expected array, got {other:?}"
            )))
        }
    };
    let elements = cast_array_elements(array.elements(), source, target_element, engine, control)?;
    let array = rebuild_array(array, elements, control)?
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions changed during cast".into()))?;
    let (array, memory) = array.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn cast_array_elements(
    values: &[Value],
    source: Option<&str>,
    target: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = match value {
            Value::List(values) => {
                let (values, memory) =
                    cast_array_elements(values, source, target, engine, control)?.into_parts();
                control.finish(Value::List(values), memory)?
            }
            value => cast_value_with_type_resolution_with_control(
                value, source, target, engine, control,
            )?,
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

pub(super) fn rebuild_array(
    source: &ArrayValue,
    elements: Produced<Vec<Value>>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ArrayValue>>> {
    let mut bounds = ProductionVec::new(*control);
    bounds.reserve(source.lower_bounds().len())?;
    for bound in source.lower_bounds() {
        bounds.push_copy(*bound)?;
    }
    Ok(ArrayValue::with_lower_bounds_with_control(
        elements,
        bounds.finish()?,
        control,
    )?)
}

#[cfg(test)]
mod tests;
