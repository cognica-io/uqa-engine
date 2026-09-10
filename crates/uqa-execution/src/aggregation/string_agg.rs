//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered string aggregation retains each row's evaluated delimiter.

use uqa_core::Value;
use uqa_sql::SQLError;

pub(super) fn finish(values: &[Value]) -> Result<Value, SQLError> {
    let Some(first) = values.first() else {
        return Ok(Value::Null);
    };
    let binary = matches!(pair(first)?.0, Value::Bytes(_));
    let mut output = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let (value, delimiter) = pair(value)?;
        if index > 0 {
            append(&mut output, delimiter, binary)?;
        }
        append(&mut output, value, binary)?;
    }
    if binary {
        Ok(Value::Bytes(output))
    } else {
        String::from_utf8(output).map(Value::Str).map_err(|error| {
            SQLError::Internal(format!("string_agg produced invalid UTF-8: {error}"))
        })
    }
}

fn pair(value: &Value) -> Result<(&Value, &Value), SQLError> {
    if let Value::List(values) = value {
        if let [value, delimiter] = values.as_slice() {
            return Ok((value, delimiter));
        }
    }
    Err(SQLError::Internal(
        "string_agg lost its value and delimiter pair".into(),
    ))
}

fn append(output: &mut Vec<u8>, value: &Value, binary: bool) -> Result<(), SQLError> {
    match value {
        Value::Null => {}
        Value::Bytes(value) if binary => output.extend(value),
        Value::Str(value) if !binary => output.extend(value.as_bytes()),
        Value::FixedChar(value) if !binary => output.extend(value.trim_end_matches(' ').as_bytes()),
        value => {
            return Err(SQLError::TypeMismatch(format!(
                "string_agg requires {} inputs, got {value:?}",
                if binary { "bytea" } else { "text" }
            )))
        }
    }
    Ok(())
}
