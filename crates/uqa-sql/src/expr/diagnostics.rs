//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`-compatible scalar type and routine diagnostics.

use uqa_core::{TemporalValue, Value};

use crate::error::SQLError;

pub fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "unknown",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "double precision",
        Value::Str(_) => "text",
        Value::FixedChar(_) => "character",
        Value::Bytes(_) => "bytea",
        Value::Temporal(TemporalValue::Interval { .. }) => "interval",
        Value::Temporal(_) => "timestamp",
        Value::Decimal(_) => "numeric",
        Value::Json(_) => "json",
        Value::JsonB(_) => "jsonb",
        Value::Array(_) => "anyarray",
        Value::List(_) => "anyarray",
        Value::Row(_) | Value::Record(_) => "record",
        Value::Map(_) => "jsonb",
    }
}

/// `function name(arg types) does not exist` - the error `PostgreSQL`
/// raises when call resolution fails (SQLSTATE 42883).
pub fn unknown_function_error(name: &str, args: &[(Option<String>, Value)]) -> SQLError {
    let types = args
        .iter()
        .map(|(arg_name, value)| match arg_name {
            Some(arg_name) => format!("{arg_name} => {}", value_type_name(value)),
            None => value_type_name(value).to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({types}) does not exist"),
    }
}
