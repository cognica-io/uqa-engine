//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared range casts select input independently of observing an existing physical value.

use super::{ColumnType, EngineHook, Produced, ProductionControl, Result, SQLError, Value};

pub(super) fn cast(
    engine: &dyn EngineHook,
    value: &Value,
    source: Option<&ColumnType>,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    let oid = match target {
        ColumnType::DeclaredRange(reference) => reference.oid,
        ColumnType::DeclaredMultirange(reference) => reference.oid,
        _ => return Ok(None),
    };
    let same_type = |ty: &ColumnType| {
        crate::catalog::type_metadata::pg_type_oid(base_type(ty)) == i64::from(oid)
    };
    match value {
        Value::Null if source.is_none_or(|ty| is_string(ty) || same_type(ty)) => {
            return Ok(Some(control.copy_value(value)?));
        }
        Value::Str(text) | Value::FixedChar(text) if source.is_none_or(is_string) => {
            return engine.read_range_input(oid, text, control).map(Some);
        }
        Value::Datum(datum) if datum.type_oid() == oid && source.is_none_or(same_type) => {
            return Ok(Some(control.copy_value(value)?))
        }
        Value::Datum(datum)
            if source.is_some_and(|ty| {
                matches!(ty, ColumnType::Domain { .. })
                    && crate::catalog::type_metadata::pg_type_oid(ty) == i64::from(datum.type_oid())
                    && crate::catalog::type_metadata::pg_type_oid(base_type(ty)) == i64::from(oid)
            }) =>
        {
            return Ok(Some(
                control.copy_value(&Value::Datum(datum.field(oid, datum.offset())))?,
            ));
        }
        _ => {}
    }
    Err(SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!(
            "cannot cast type {} to {}",
            source.map_or_else(|| "unknown".into(), ColumnType::display_name),
            target.display_name()
        ),
    })
}

fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

fn is_string(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::Text | ColumnType::Varchar(_) | ColumnType::Character(_) | ColumnType::Name
    )
}
