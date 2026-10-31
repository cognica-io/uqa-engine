//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine conversion owns type lookup and result construction under one production control.

use super::{RoutineValueContext, RoutineValueDescription, RoutineValueKind};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RecordValue, Value,
};

pub fn coerce_routine_value_from_with_control(
    context: &dyn RoutineValueContext,
    value: Produced<Value>,
    type_name: &str,
    source: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    match RoutineValueDescription::resolve_with_control(context, type_name, control)? {
        Some(description) => description.coerce_with_control(context, value, source, control),
        None => crate::expr::cast_value_from_with_control(&value, type_name, None, control),
    }
}

impl RoutineValueDescription {
    pub fn resolve_with_control(
        context: &dyn RoutineValueContext,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, SQLError> {
        let canonical =
            crate::type_resolution::canonical_routine_type_name_with_control(name, control)?;
        let kind = match canonical.as_str() {
            "record" => RoutineValueKind::Record,
            "trigger" => RoutineValueKind::Trigger,
            "anyarray" => RoutineValueKind::AnyArray,
            "refcursor" => RoutineValueKind::RefCursor,
            "void" => RoutineValueKind::Void,
            _ => {
                let Some(ty) = context.catalog_column_type_with_control(name, control)? else {
                    return Ok(None);
                };
                let (ty, memory) = ty.into_parts();
                return Ok(Some(
                    control.finish(Self(RoutineValueKind::Catalog(ty)), memory)?,
                ));
            }
        };
        Ok(Some(
            control.finish(Self(kind), control.empty_reservation())?,
        ))
    }

    pub fn coerce_with_control(
        &self,
        context: &dyn RoutineValueContext,
        value: Produced<Value>,
        source: Option<&ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        let (value, memory) = value.into_parts();
        let value = control.finish(value, memory)?;
        match &self.0 {
            RoutineValueKind::Record => match &*value {
                Value::Record(_) | Value::Null => Ok(value),
                Value::Datum(datum)
                    if crate::expr::composites::datum_is_composite_with_control(
                        datum, context, control,
                    )? =>
                {
                    Ok(value)
                }
                Value::Row(row) if row.field_types().is_some() => Ok(value),
                Value::Row(row) => record(row.values(), control),
                _ => Err(type_error("cannot cast non-composite value to type record")),
            },
            RoutineValueKind::Trigger => match &*value {
                Value::Record(_) | Value::Row(_) | Value::Null => Ok(value),
                Value::Datum(datum)
                    if crate::expr::composites::datum_is_composite_with_control(
                        datum, context, control,
                    )? =>
                {
                    Ok(value)
                }
                _ => Err(type_error("trigger function must return a row or NULL")),
            },
            RoutineValueKind::AnyArray => match &*value {
                Value::Array(_) | Value::LegacyVector(_) | Value::List(_) | Value::Null => {
                    Ok(value)
                }
                _ => Err(type_error("cannot cast non-array value to type anyarray")),
            },
            RoutineValueKind::RefCursor => match &*value {
                Value::Str(_) | Value::Null => Ok(value),
                _ => Err(type_error("cannot cast value to type refcursor")),
            },
            RoutineValueKind::Void if matches!(*value, Value::Null) => Ok(value),
            RoutineValueKind::Void => Err(type_error("cannot cast non-null value to type void")),
            RoutineValueKind::Catalog(target) => {
                crate::assignment::conversion::coerce_assignment_value_with_control(
                    context, value, target, source, control,
                )
            }
        }
    }
}

fn type_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: message.into(),
    }
}

fn record(values: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>, SQLError> {
    let mut fields = ProductionVec::new(*control);
    fields.reserve(values.len())?;
    for (index, value) in values.iter().enumerate() {
        let name = control.format(format_args!("f{}", index + 1))?;
        let value = control.copy_value(value)?;
        let (name, name_memory) = name.into_parts();
        let (value, value_memory) = value.into_parts();
        fields.push_produced(
            control.finish((name, value), control.combine(name_memory, value_memory))?,
        )?;
    }
    let (record, memory) = RecordValue::with_control(fields.finish()?, None, control)?.into_parts();
    Ok(control.finish(Value::Record(record), memory)?)
}

#[cfg(test)]
mod tests;
