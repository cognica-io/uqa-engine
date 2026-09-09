//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed `PostgreSQL` text output for result consumers.

use std::fmt::Write;

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::{format_regtype_value, value_to_string, vector_value_to_string, EngineHook};
use uqa_sql::SQLError;

/// Format a non-NULL result value using its declared type and optional catalog resolver. NULL remains separate from text in the calling result protocol.
pub fn format_postgres_text(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    if let ColumnType::Domain { base, .. } = ty {
        return format_postgres_text(value, base, engine);
    }
    if let Some(text) = format_regtype_value(value, ty, engine)? {
        return Ok(text);
    }
    if matches!(ty, ColumnType::Int2Vector | ColumnType::OidVector) {
        return vector_value_to_string(value)
            .ok_or_else(|| SQLError::Internal("invalid catalog vector result carrier".into()));
    }
    if let ColumnType::Array(element) = ty {
        return format_array(value, element, engine);
    }
    Ok(match value {
        Value::Bool(value) => if *value { "t" } else { "f" }.into(),
        Value::FixedChar(value) => value.clone(),
        Value::Float(value) if matches!(ty, ColumnType::Real) => {
            uqa_sql::expr::format_real(*value as f32)
        }
        Value::Float(value) => uqa_graph::agtype::format_float_pg(*value),
        _ => value_to_string(value),
    })
}

fn format_array(
    value: &Value,
    element: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    let (values, prefix) = match value {
        Value::Array(array) => {
            let mut prefix = String::new();
            if array.lower_bounds().iter().any(|lower| *lower != 1) {
                for (lower, length) in array.lower_bounds().iter().zip(array.dimensions()) {
                    let upper = i64::from(*lower) + *length as i64 - 1;
                    write!(prefix, "[{lower}:{upper}]").expect("writing to String cannot fail");
                }
                prefix.push('=');
            }
            (array.elements(), prefix)
        }
        Value::List(values) => (values.as_slice(), String::new()),
        _ => return Err(SQLError::Internal("invalid array result carrier".into())),
    };
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        fields.push(match value {
            Value::Null => "NULL".into(),
            Value::List(_) | Value::Array(_) => format_array(value, element, engine)?,
            _ => {
                let text = format_postgres_text(value, element, engine)?;
                if text.is_empty()
                    || text.eq_ignore_ascii_case("null")
                    || text.chars().any(|c| {
                        c.is_ascii_whitespace() || matches!(c, ',' | '{' | '}' | '"' | '\\')
                    })
                {
                    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    text
                }
            }
        });
    }
    Ok(format!("{prefix}{{{}}}", fields.join(",")))
}
