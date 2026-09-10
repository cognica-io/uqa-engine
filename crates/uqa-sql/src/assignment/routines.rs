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
    match canonical_routine_type_name(type_name).as_str() {
        "record" => match value {
            Value::Record(_) | Value::Null => Ok(value.clone()),
            Value::Row(values) => Ok(Value::Record(
                values
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| (format!("f{}", index + 1), value))
                    .collect(),
            )),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast non-composite value to type record".into(),
            }),
        },
        "trigger" => match value {
            Value::Record(_) | Value::Row(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "trigger function must return a row or NULL".into(),
            }),
        },
        "anyarray" => match value {
            Value::Array(_) | Value::List(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast non-array value to type anyarray".into(),
            }),
        },
        "refcursor" => match value {
            Value::Str(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast value to type refcursor".into(),
            }),
        },
        "void" if matches!(value, Value::Null) => Ok(Value::Null),
        "void" => Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "cannot cast non-null value to type void".into(),
        }),
        _ => {
            if let Some(target) = context.catalog_column_type(type_name) {
                return crate::assignment::conversion::coerce_assignment_value(
                    context,
                    value.clone(),
                    &target,
                    source,
                );
            }
            cast_value(value, type_name)
        }
    }
}
