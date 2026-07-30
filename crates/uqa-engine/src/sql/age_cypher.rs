//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE-compatible SQL table-function adapter for Cypher.
//!
//! The graph crate owns parsing and execution. This module maps
//! PostgreSQL-style `FROM cypher(...) AS (...)` calls onto the
//! engine's registered graph workspaces and coerces each output
//! column to its declared SQL type exactly like AGE 1.6.0:
//! `agtype` columns carry canonical agtype text (vertices render as
//! `{...}::vertex`, strings stay JSON-quoted), scalar columns coerce
//! with AGE's cast rules (float -> int rounds half-to-even, strings
//! re-parse through agtype, entities refuse to cast).

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_graph::agtype;
use uqa_sql::{ResultRow, SQLError};

use crate::Engine;

pub(super) fn build_rows(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluated: &[Value],
    qualifier: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
    if !(2..=3).contains(&evaluated.len()) {
        return Err(SQLError::TypeMismatch(
            "cypher requires 2-3 args (graph_name, query_string[, parameters])".into(),
        ));
    }
    if column_aliases.is_empty() {
        return Err(SQLError::TypeMismatch(
            "cypher requires a record definition: AS (column agtype, ...)".into(),
        ));
    }
    if args.len() == 3 && !is_valid_parameter_expr(&args[2]) {
        return Err(SQLError::TypeMismatch(
            "cypher parameters must be supplied through an SQL parameter".into(),
        ));
    }

    let graph = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => {
            return Err(SQLError::TypeMismatch(
                "cypher.graph_name must be string".into(),
            ))
        }
    };
    let query = match &evaluated[1] {
        Value::Str(s) => s.clone(),
        _ => {
            return Err(SQLError::TypeMismatch(
                "cypher.query_string must be string".into(),
            ))
        }
    };
    if !engine
        .has_graph(&graph)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{graph}\" does not exist"
        )));
    }

    let params = match evaluated.get(2) {
        Some(value) => parameter_map(value)?,
        None => BTreeMap::new(),
    };
    let (cypher_columns, cypher_rows) = engine
        .run_cypher(&graph, &query, params)
        .map_err(|e| SQLError::Unsupported(format!("cypher: {e}")))?;
    if !cypher_columns.is_empty() && cypher_columns.len() != column_aliases.len() {
        return Err(SQLError::TypeMismatch(
            "return row and column definition list do not match".into(),
        ));
    }

    let mut out = Vec::with_capacity(cypher_rows.len());
    for src in cypher_rows {
        let mut row = ResultRow::new();
        for (idx, target_col) in column_aliases.iter().enumerate() {
            let value = cypher_columns
                .get(idx)
                .and_then(|source_col| src.get(source_col))
                .cloned()
                .unwrap_or(Value::Null);
            let declared = column_types.get(idx).map_or("agtype", String::as_str);
            let value = coerce_to_column_type(value, declared, target_col)?;
            row.insert(target_col.clone(), value);
        }
        let row = match qualifier {
            Some(alias) => super::prefix_row(alias, &row),
            None => row,
        };
        out.push(row);
    }
    Ok(out)
}

/// Coerce one cypher output value to the SQL type declared in the
/// record definition, following AGE's cast behavior.
fn coerce_to_column_type(value: Value, declared: &str, column: &str) -> Result<Value, SQLError> {
    match declared {
        // No type available (plain alias list) behaves like agtype.
        "agtype" | "" => Ok(match value {
            // Top-level SQL NULL stays NULL (renders empty in psql).
            Value::Null => Value::Null,
            other => Value::Str(agtype::render(&other)),
        }),
        "int2" | "smallint" => coerce_int(value, i64::from(i16::MIN), i64::from(i16::MAX)),
        "int4" | "int" | "integer" => coerce_int(value, i64::from(i32::MIN), i64::from(i32::MAX)),
        "int8" | "bigint" => coerce_int(value, i64::MIN, i64::MAX),
        "float4" | "float8" | "float" | "real" | "double precision" => coerce_float(value),
        "text" | "varchar" | "bpchar" | "char" | "name" => coerce_text(value),
        "bool" | "boolean" => coerce_bool(value),
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast type agtype to {other} for column \"{column}\""
        ))),
    }
}

/// Re-parse a string operand as an agtype scalar, mirroring AGE's
/// string-to-agtype cast path (`'42'::int` works, `'abc'::int` raises
/// `invalid input syntax for type agtype`).
fn parse_agtype_scalar(s: &str) -> Result<Value, SQLError> {
    let trimmed = s.trim();
    if let Ok(n) = trimmed.parse::<i64>() {
        return Ok(Value::Int(n));
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        return Ok(Value::Float(f));
    }
    match trimmed {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "null" => Ok(Value::Null),
        _ => Err(SQLError::TypeMismatch(format!(
            "invalid input syntax for type agtype: expected agtype value, but found \"{s}\""
        ))),
    }
}

fn coerce_int(value: Value, min: i64, max: i64) -> Result<Value, SQLError> {
    let out = match value {
        Value::Null => return Ok(Value::Null),
        Value::Int(n) => n,
        // PostgreSQL float -> int casts round half to even.
        Value::Float(f) => {
            let rounded = f.round_ties_even();
            let above_max = if max == i64::MAX {
                rounded >= 9_223_372_036_854_775_808.0
            } else {
                rounded > max as f64
            };
            if !rounded.is_finite() || rounded < min as f64 || above_max {
                return Err(SQLError::TypeMismatch("integer out of range".into()));
            }
            rounded as i64
        }
        Value::Bool(b) => i64::from(b),
        Value::Str(s) => {
            return coerce_int(parse_agtype_scalar(&s)?, min, max);
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast agtype {} to type integer",
                agtype::agtype_type_name(&other)
            )));
        }
    };
    if out < min || out > max {
        return Err(SQLError::TypeMismatch("integer out of range".into()));
    }
    Ok(Value::Int(out))
}

fn coerce_float(value: Value) -> Result<Value, SQLError> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(n) => Ok(Value::Float(n as f64)),
        Value::Float(f) => Ok(Value::Float(f)),
        Value::Str(s) => coerce_float(parse_agtype_scalar(&s)?),
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast agtype {} to type float",
            agtype::agtype_type_name(&other)
        ))),
    }
}

fn coerce_text(value: Value) -> Result<Value, SQLError> {
    match value {
        Value::Null => Ok(Value::Null),
        // Strings pass through raw (no JSON quoting) as text.
        Value::Str(s) => Ok(Value::Str(s)),
        other => {
            // AGE refuses to cast graph entities to text.
            if agtype::entity_kind(&other).is_some() {
                return Err(SQLError::TypeMismatch(format!(
                    "agtype_value_to_text: unsupported argument agtype {}",
                    agtype::agtype_type_ordinal(&other)
                )));
            }
            Ok(Value::Str(agtype::render(&other)))
        }
    }
}

fn coerce_bool(value: Value) -> Result<Value, SQLError> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Bool(b) => Ok(Value::Bool(b)),
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast agtype {} to type boolean",
            agtype::agtype_type_name(&other)
        ))),
    }
}

fn is_valid_parameter_expr(expr: &ScalarExpr) -> bool {
    matches!(
        expr,
        ScalarExpr::Param(_) | ScalarExpr::Literal(Value::Null)
    )
}

fn parameter_map(value: &Value) -> Result<BTreeMap<String, Value>, SQLError> {
    match value {
        Value::Null => Ok(BTreeMap::new()),
        Value::Map(map) => Ok(map.clone()),
        Value::Str(s) => {
            let parsed = serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid cypher parameters: {e}")))?;
            match super::json_to_core_value(parsed) {
                Value::Map(map) => Ok(map),
                _ => Err(SQLError::TypeMismatch(
                    "cypher parameters must be a map".into(),
                )),
            }
        }
        _ => Err(SQLError::TypeMismatch(
            "cypher parameters must be a map".into(),
        )),
    }
}
