//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLSTATE matching, row diagnostics, and `RAISE` formatting.

use super::{cast_value, condition_sqlstates, ResultRow, SQLError, SQLResult, Value};

pub(super) fn return_query_context_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "cannot use RETURN QUERY in a non-SETOF function".into(),
    }
}

/// Column lookup with a qualified-key fallback: result rows may key
/// values as `table.column` while the column list carries the bare
/// label.
pub(super) fn row_value(row: &ResultRow, column: &str) -> Value {
    if let Some(value) = row.get(column) {
        return value.clone();
    }
    row.iter()
        .find(|(key, _)| {
            key.rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix == column)
        })
        .map_or(Value::Null, |(_, value)| value.clone())
}

pub(super) fn result_row_count(result: &SQLResult) -> Result<i64, SQLError> {
    let (raw_count, source) = if result.columns.is_empty() {
        (result.affected_rows, "affected-row")
    } else {
        (
            u64::try_from(result.rows.len()).map_err(|_| {
                SQLError::Internal(format!(
                    "result row count {} cannot be represented as u64",
                    result.rows.len()
                ))
            })?,
            "result-row",
        )
    };
    i64::try_from(raw_count).map_err(|_| {
        SQLError::Internal(format!(
            "{source} count {raw_count} exceeds PL/pgSQL's signed 64-bit ROW_COUNT range"
        ))
    })
}

pub(super) fn strict_into_check(row_count: i64) -> Result<(), SQLError> {
    if row_count == 0 {
        return Err(SQLError::Routine {
            sqlstate: "P0002".into(),
            message: "query returned no rows".into(),
        });
    }
    if row_count > 1 {
        return Err(SQLError::Routine {
            sqlstate: "P0003".into(),
            message: "query returned more than one row".into(),
        });
    }
    Ok(())
}

pub(super) fn to_i64_value(value: &Value) -> Result<i64, SQLError> {
    match cast_value(value, "bigint")? {
        Value::Int(v) => Ok(v),
        other => Err(SQLError::TypeMismatch(format!(
            "expected an integer, got {other:?}"
        ))),
    }
}

pub(super) fn catchable(error: &SQLError) -> bool {
    !matches!(error, SQLError::Cancelled(_))
}

/// Message text exposed through SQLERRM: user-routine errors keep
/// their raw message, engine errors keep their display form.
pub(super) fn routine_message(error: &SQLError) -> String {
    match error {
        SQLError::Routine { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

pub(super) fn looks_like_sqlstate(text: &str) -> bool {
    text.len() == 5 && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Match an exception arm's condition list against a `SQLSTATE`.
pub(super) fn arm_matches(conditions: &[String], state: &str) -> Result<bool, SQLError> {
    for condition in conditions {
        if condition == "others" {
            // WHEN OTHERS catches everything except QUERY_CANCELED
            // and ASSERT_FAILURE, matching PostgreSQL.
            if state != "57014" && state != "P0004" {
                return Ok(true);
            }
            continue;
        }
        let mut known_condition = false;
        for mapped in condition_sqlstates(condition) {
            known_condition = true;
            if sqlstate_matches(mapped, state) {
                return Ok(true);
            }
        }
        if !known_condition {
            if looks_like_sqlstate(condition) {
                if sqlstate_matches(&condition.to_ascii_uppercase(), state) {
                    return Ok(true);
                }
            } else {
                return Err(SQLError::Internal(format!(
                    "unrecognized PL/pgSQL exception condition `{condition}`"
                )));
            }
        }
    }
    Ok(false)
}

pub(super) fn sqlstate_matches(condition: &str, state: &str) -> bool {
    condition == state || (condition.ends_with("000") && state.get(..2) == condition.get(..2))
}

/// Substitute `%` placeholders in a RAISE format string.
pub(super) fn format_raise_message(format: &str, args: &[Value]) -> Result<String, SQLError> {
    let mut out = String::with_capacity(format.len() + 16);
    let mut chars = format.chars().peekable();
    let mut next_arg = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let Some(value) = args.get(next_arg) else {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "too few parameters specified for RAISE".into(),
            });
        };
        next_arg += 1;
        out.push_str(&raise_text(value));
    }
    if next_arg < args.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "too many parameters specified for RAISE".into(),
        });
    }
    Ok(out)
}

/// Text form of a value inside a RAISE message (`NULL` renders as
/// `<NULL>`, booleans as `t` / `f`, arrays in brace form).
pub(super) fn raise_text(value: &Value) -> String {
    match value {
        Value::Null => "<NULL>".into(),
        Value::Bool(b) => (if *b { "t" } else { "f" }).into(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Decimal(v) => v.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::FixedChar(s) => s.trim_end_matches(' ').to_string(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Bytes(b) => {
            use std::fmt::Write as _;
            let mut out = String::with_capacity(2 + b.len() * 2);
            out.push_str("\\x");
            for byte in b {
                let _ = write!(out, "{byte:02x}");
            }
            out
        }
        Value::List(items) => {
            let inner = items.iter().map(raise_text).collect::<Vec<_>>().join(",");
            format!("{{{inner}}}")
        }
        Value::Map(map) => {
            let inner = map.values().map(raise_text).collect::<Vec<_>>().join(",");
            format!("({inner})")
        }
    }
}
