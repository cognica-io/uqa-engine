//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::ColumnType, expr::cast_value, type_resolution::canonical_routine_type_name, SQLError,
};
use uqa_core::Value;
pub trait RoutineValueContext: super::AssignmentContext {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType>;
}
pub fn coerce_routine_value(
    context: &dyn RoutineValueContext,
    value: &Value,
    type_name: &str,
) -> Result<Value, SQLError> {
    coerce_routine_value_from(context, value, type_name, None)
}

pub fn coerce_routine_value_from(
    context: &dyn RoutineValueContext,
    value: &Value,
    type_name: &str,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    match RoutineValueDescription::resolve(context, type_name) {
        Some(description) => description.coerce(context, value, source),
        None => cast_value(value, type_name),
    }
}

/// The assignment semantics selected by a routine's declared type. This describes a conversion, never a converted value; callers may retain it only while the catalog and name-resolution scope that selected it remain unchanged.
#[derive(Debug)]
pub struct RoutineValueDescription(RoutineValueKind);

#[derive(Debug)]
enum RoutineValueKind {
    Record,
    Trigger,
    AnyArray,
    RefCursor,
    Void,
    Catalog(ColumnType),
}

impl RoutineValueDescription {
    /// Resolve at the original conversion boundary. An unresolved name keeps the ordinary value-dependent cast path and is not a reusable description.
    pub fn resolve(context: &dyn RoutineValueContext, name: &str) -> Option<Self> {
        Some(Self(match canonical_routine_type_name(name).as_str() {
            "record" => RoutineValueKind::Record,
            "trigger" => RoutineValueKind::Trigger,
            "anyarray" => RoutineValueKind::AnyArray,
            "refcursor" => RoutineValueKind::RefCursor,
            "void" => RoutineValueKind::Void,
            _ => RoutineValueKind::Catalog(context.catalog_column_type(name)?),
        }))
    }

    /// Admit a retained copy of the descriptor's owned type payload before copying it.
    pub fn clone_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, uqa_core::ValueRetentionError> {
        let kind = match &self.0 {
            RoutineValueKind::Record => RoutineValueKind::Record,
            RoutineValueKind::Trigger => RoutineValueKind::Trigger,
            RoutineValueKind::AnyArray => RoutineValueKind::AnyArray,
            RoutineValueKind::RefCursor => RoutineValueKind::RefCursor,
            RoutineValueKind::Void => RoutineValueKind::Void,
            RoutineValueKind::Catalog(target) => {
                let (target, memory) = target.clone_with_control(control)?.into_parts();
                return control.finish(Self(RoutineValueKind::Catalog(target)), memory);
            }
        };
        control.finish(Self(kind), control.empty_reservation())
    }

    /// Apply the original assignment kernel with live input/domain callbacks and session settings. The caller must revalidate its scope after evaluating the input and any preceding callback.
    pub fn coerce(
        &self,
        context: &dyn RoutineValueContext,
        value: &Value,
        source: Option<&ColumnType>,
    ) -> Result<Value, SQLError> {
        match &self.0 {
            RoutineValueKind::Record => match value {
                Value::Record(_) | Value::Null => Ok(value.clone()),
                Value::Datum(datum)
                    if crate::expr::composites::datum_is_composite(datum, context)? =>
                {
                    Ok(value.clone())
                }
                Value::Row(row) if row.field_types().is_some() => Ok(value.clone()),
                Value::Row(values) => Ok(Value::Record(
                    values
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(index, value)| (format!("f{}", index + 1), value))
                        .collect::<Vec<_>>()
                        .into(),
                )),
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot cast non-composite value to type record".into(),
                }),
            },
            RoutineValueKind::Trigger => match value {
                Value::Record(_) | Value::Row(_) | Value::Null => Ok(value.clone()),
                Value::Datum(datum)
                    if crate::expr::composites::datum_is_composite(datum, context)? =>
                {
                    Ok(value.clone())
                }
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "trigger function must return a row or NULL".into(),
                }),
            },
            RoutineValueKind::AnyArray => match value {
                Value::Array(_) | Value::LegacyVector(_) | Value::List(_) | Value::Null => {
                    Ok(value.clone())
                }
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot cast non-array value to type anyarray".into(),
                }),
            },
            RoutineValueKind::RefCursor => match value {
                Value::Str(_) | Value::Null => Ok(value.clone()),
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot cast value to type refcursor".into(),
                }),
            },
            RoutineValueKind::Void if matches!(value, Value::Null) => Ok(Value::Null),
            RoutineValueKind::Void => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast non-null value to type void".into(),
            }),
            RoutineValueKind::Catalog(target) => {
                crate::assignment::conversion::coerce_assignment_value(
                    context,
                    value.clone(),
                    target,
                    source,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
