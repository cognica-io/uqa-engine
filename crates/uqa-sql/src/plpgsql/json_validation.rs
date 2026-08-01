//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parser JSON shape, datum-kind, and numeric conversion validation.

use super::{JSONValue, PLpgSQLDatum, Result, SQLError};

pub(super) fn normalize_plpgsql_type(raw: &str) -> String {
    let mut t = raw.trim().to_ascii_lowercase();
    if let Some(rest) = t.strip_prefix("pg_catalog.") {
        t = rest.to_string();
    }
    t.replace('"', "")
}

pub(super) fn require<'a>(obj: &'a JSONValue, key: &str) -> Result<&'a JSONValue> {
    obj.get(key)
        .ok_or_else(|| SQLError::Internal(format!("PL/pgSQL node missing `{key}`")))
}

pub(super) fn ensure_single_tag(raw: &JSONValue, context: &str) -> Result<()> {
    let object = raw
        .as_object()
        .ok_or_else(|| SQLError::Internal(format!("PL/pgSQL {context} node is not an object")))?;
    if object.len() != 1 {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL {context} node must contain exactly one tag, found {}",
            object.len()
        )));
    }
    Ok(())
}

pub(super) fn expect_tag<'a>(
    raw: &'a JSONValue,
    tag: &str,
    context: &str,
) -> Result<&'a JSONValue> {
    ensure_single_tag(raw, context)?;
    raw.get(tag).ok_or_else(|| {
        SQLError::Internal(format!(
            "PL/pgSQL {context} expected `{tag}`, got `{}`",
            json_kind(raw)
        ))
    })
}

pub(super) fn require_nonempty_str(obj: &JSONValue, key: &str, context: &str) -> Result<String> {
    match obj.get(key) {
        Some(JSONValue::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(JSONValue::String(_)) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} has an empty `{key}`"
        ))),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} `{key}` must be a string, got {other}"
        ))),
        None => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} is missing `{key}`"
        ))),
    }
}

pub(super) fn json_optional_str(obj: &JSONValue, key: &str) -> Result<Option<String>> {
    match obj.get(key) {
        Some(JSONValue::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be a string, got {other}"
        ))),
        None => Ok(None),
    }
}

pub(super) fn json_bool_or_false(obj: &JSONValue, key: &str) -> Result<bool> {
    match obj.get(key) {
        Some(JSONValue::Bool(value)) => Ok(*value),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be a boolean, got {other}"
        ))),
        None => Ok(false),
    }
}

pub(super) fn require_i64(obj: &JSONValue, key: &str, context: &str) -> Result<i64> {
    match obj.get(key) {
        Some(value) => value.as_i64().ok_or_else(|| {
            SQLError::Internal(format!(
                "PL/pgSQL {context} `{key}` must be a signed integer, got {value}"
            ))
        }),
        None => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} is missing `{key}`"
        ))),
    }
}

pub(super) fn json_optional_i64(obj: &JSONValue, key: &str) -> Result<Option<i64>> {
    match obj.get(key) {
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            SQLError::Internal(format!(
                "PL/pgSQL `{key}` must be a signed integer, got {value}"
            ))
        }),
        None => Ok(None),
    }
}

pub(super) fn json_i64_or_zero(obj: &JSONValue, key: &str) -> Result<i64> {
    Ok(json_optional_i64(obj, key)?.unwrap_or(0))
}

pub(super) fn json_optional_usize(obj: &JSONValue, key: &str) -> Result<Option<usize>> {
    match obj.get(key) {
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                SQLError::Internal(format!(
                    "PL/pgSQL `{key}` must be a non-negative integer, got {value}"
                ))
            })?;
            usize::try_from(raw).map(Some).map_err(|_| {
                SQLError::Internal(format!(
                    "PL/pgSQL `{key}` value {raw} does not fit this platform"
                ))
            })
        }
        None => Ok(None),
    }
}

pub(super) fn json_usize_or_zero(obj: &JSONValue, key: &str) -> Result<usize> {
    Ok(json_optional_usize(obj, key)?.unwrap_or(0))
}

pub(super) fn optional_array<'a>(obj: &'a JSONValue, key: &str) -> Result<Option<&'a [JSONValue]>> {
    match obj.get(key) {
        Some(JSONValue::Array(values)) => Ok(Some(values)),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be an array, got {other}"
        ))),
        None => Ok(None),
    }
}

pub(super) fn validate_datum<'a>(
    datums: &'a [PLpgSQLDatum],
    index: usize,
    context: &str,
) -> Result<&'a PLpgSQLDatum> {
    datums.get(index).ok_or_else(|| {
        SQLError::Internal(format!(
            "PL/pgSQL {context} references missing datum {index}"
        ))
    })
}

pub(super) fn validate_assignable_datum(
    datums: &[PLpgSQLDatum],
    index: usize,
    context: &str,
) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Var(_) | PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::RecField { .. } => Ok(()),
        PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is a row container, not an assignable value"
        ))),
    }
}

pub(super) fn validate_scalar_datum(
    datums: &[PLpgSQLDatum],
    index: usize,
    context: &str,
) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Var(_) | PLpgSQLDatum::RecField { .. } => Ok(()),
        PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is not scalar"
        ))),
    }
}

pub(super) fn validate_record_datum(
    datums: &[PLpgSQLDatum],
    index: usize,
    context: &str,
) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Rec { .. } => Ok(()),
        _ => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is not a record"
        ))),
    }
}

pub(super) fn json_kind(obj: &JSONValue) -> String {
    obj.as_object()
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| "<unknown>".into())
}

// ---------------------------------------------------------------------
// Variable binding
// ---------------------------------------------------------------------
