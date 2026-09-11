//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Argument contracts for analyzer and session table functions.

use crate::SQLError;
use uqa_core::Value;

pub fn require_no_arguments(name: &str, values: &[Value]) -> Result<(), SQLError> {
    if values.is_empty() {
        return Ok(());
    }
    Err(SQLError::BadArity {
        name: name.into(),
        expected: "0".into(),
        actual: values.len(),
    })
}

pub fn create_analyzer_arguments(evaluated: &[Value]) -> Result<(String, String), SQLError> {
    if evaluated.len() != 2 {
        return Err(SQLError::BadArity {
            name: "create_analyzer".into(),
            expected: "2".into(),
            actual: evaluated.len(),
        });
    }
    let analyzer_name = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("create_analyzer arg 1".into())),
    };
    let config_json = match &evaluated[1] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("create_analyzer arg 2".into())),
    };
    Ok((analyzer_name, config_json))
}

pub fn drop_analyzer_arguments(evaluated: &[Value]) -> Result<String, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::BadArity {
            name: "drop_analyzer".into(),
            expected: "1".into(),
            actual: evaluated.len(),
        });
    }
    let analyzer_name = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
    };
    Ok(analyzer_name)
}

pub fn set_table_analyzer_arguments(
    evaluated: &[Value],
) -> Result<(String, String, String, String), SQLError> {
    if !(3..=4).contains(&evaluated.len()) {
        return Err(SQLError::BadArity {
            name: "set_table_analyzer".into(),
            expected: "3 or 4".into(),
            actual: evaluated.len(),
        });
    }
    let target_table = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 1".into())),
    };
    let field = match &evaluated[1] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 2".into())),
    };
    let analyzer_name = match &evaluated[2] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 3".into())),
    };
    let phase = if evaluated.len() > 3 {
        match &evaluated[3] {
            Value::Str(s) => s.clone(),
            _ => {
                return Err(SQLError::TypeMismatch(
                    "set_table_analyzer phase must be a string".into(),
                ));
            }
        }
    } else {
        "both".into()
    };
    Ok((target_table, field, analyzer_name, phase))
}

pub fn fts_index_stats_table(evaluated: &[Value]) -> Result<Option<&str>, SQLError> {
    if evaluated.len() > 1 {
        return Err(SQLError::TypeMismatch(
            "fts_index_stats accepts optional table name".into(),
        ));
    }
    let table_filter = match evaluated.first() {
        Some(Value::Str(s)) => Some(s.as_str()),
        Some(_) => return Err(SQLError::TypeMismatch("fts_index_stats arg 1".into())),
        None => None,
    };
    Ok(table_filter)
}
