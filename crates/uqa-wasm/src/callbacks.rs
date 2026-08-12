//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Browser JavaScript adapters for the engine SQL callback contracts.

use std::ffi::{c_char, CString};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value as JSON};
use uqa_core::Value;
use uqa_engine::{
    SQLAggregateFunction, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility,
    SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult,
};
use uqa_sql::SQLError;

use super::{value_from_json, value_to_json};

#[cfg(target_os = "emscripten")]
unsafe extern "C" {
    fn uqa_invoke_callback(callback_id: u32, request: *const c_char) -> *mut c_char;
    fn uqa_free_callback_result(ptr: *mut c_char);
}

pub(super) fn function_options(args: &JSON) -> Result<SQLFunctionOptions, String> {
    let Some(options) = args.get("options").filter(|value| !value.is_null()) else {
        return Ok(SQLFunctionOptions::default());
    };
    let options = options
        .as_object()
        .ok_or("SQL function options must be an object")?;
    let volatility = match options.get("volatility") {
        None | Some(JSON::Null) => SQLFunctionVolatility::Volatile,
        Some(JSON::String(value)) if value.eq_ignore_ascii_case("volatile") => {
            SQLFunctionVolatility::Volatile
        }
        Some(JSON::String(value)) if value.eq_ignore_ascii_case("stable") => {
            SQLFunctionVolatility::Stable
        }
        Some(JSON::String(value)) if value.eq_ignore_ascii_case("immutable") => {
            SQLFunctionVolatility::Immutable
        }
        Some(JSON::String(value)) => {
            return Err(format!(
                "unknown SQL function volatility `{value}`; expected volatile, stable, or immutable"
            ));
        }
        Some(_) => return Err("SQL function option `volatility` must be a string".to_string()),
    };
    let may_mutate_engine = match options.get("mayMutateEngine") {
        None | Some(JSON::Null) => true,
        Some(JSON::Bool(value)) => *value,
        Some(_) => {
            return Err("SQL function option `mayMutateEngine` must be a boolean".to_string());
        }
    };
    Ok(SQLFunctionOptions::new(volatility, may_mutate_engine))
}

pub(super) fn callback_id(args: &JSON) -> Result<u32, String> {
    let value = args
        .get("callbackId")
        .and_then(JSON::as_u64)
        .ok_or("SQL function registration needs an unsigned `callbackId`")?;
    u32::try_from(value).map_err(|_| "SQL callback ID exceeds the u32 range".to_string())
}

fn callback_error(name: &str, operation: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!(
        "JavaScript SQL function `{name}` failed to {operation}: {error}"
    ))
}

fn invoke(callback_id: u32, request: &JSON) -> Result<JSON, String> {
    let request = CString::new(request.to_string())
        .map_err(|_| "SQL callback request contains an interior NUL".to_string())?;
    let text = invoke_raw(callback_id, request.as_ptr())?;
    let response: JSON = serde_json::from_str(&text)
        .map_err(|error| format!("SQL callback returned invalid JSON: {error}"))?;
    if let Some(error) = response.get("error") {
        return Err(error
            .as_str()
            .map_or_else(|| error.to_string(), ToString::to_string));
    }
    response
        .get("ok")
        .cloned()
        .ok_or_else(|| "SQL callback response needs `ok` or `error`".to_string())
}

#[cfg(target_os = "emscripten")]
fn invoke_raw(callback_id: u32, request: *const c_char) -> Result<String, String> {
    let response = unsafe { uqa_invoke_callback(callback_id, request) };
    if response.is_null() {
        return Err("JavaScript SQL callback bridge returned a null response".to_string());
    }
    let text = unsafe { std::ffi::CStr::from_ptr(response) }
        .to_str()
        .map(ToString::to_string)
        .map_err(|_| "JavaScript SQL callback response is not valid UTF-8".to_string());
    unsafe { uqa_free_callback_result(response) };
    text
}

#[cfg(not(target_os = "emscripten"))]
fn invoke_raw(_callback_id: u32, _request: *const c_char) -> Result<String, String> {
    Err("JavaScript SQL callbacks require an emscripten build".to_string())
}

pub(super) struct WASMScalarFunction {
    pub(super) name: String,
    pub(super) callback_id: u32,
}

impl SQLScalarFunction for WASMScalarFunction {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        let args = args
            .iter()
            .cloned()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| callback_error(&self.name, "prepare scalar arguments", error))?;
        let result = invoke(
            self.callback_id,
            &json!({ "operation": "scalar", "args": args }),
        )
        .map_err(|error| callback_error(&self.name, "call scalar callback", error))?;
        value_from_json(&result)
            .map_err(|error| callback_error(&self.name, "convert scalar result", error))
    }
}

pub(super) struct WASMTableFunction {
    pub(super) name: String,
    pub(super) callback_id: u32,
}

impl SQLTableFunction for WASMTableFunction {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        let args = args
            .iter()
            .cloned()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| callback_error(&self.name, "prepare table arguments", error))?;
        let result = invoke(
            self.callback_id,
            &json!({ "operation": "table", "args": args }),
        )
        .map_err(|error| callback_error(&self.name, "call table callback", error))?;
        let value = value_from_json(&result)
            .map_err(|error| callback_error(&self.name, "decode table result", error))?;
        table_function_result(value)
            .map_err(|error| callback_error(&self.name, "convert table result", error))
    }
}

fn table_function_result(value: Value) -> Result<SQLTableFunctionResult, String> {
    match value {
        Value::Map(mut result) => {
            let columns = result
                .remove("columns")
                .ok_or_else(|| "table result object needs `columns`".to_string())?;
            let rows = result
                .remove("rows")
                .ok_or_else(|| "table result object needs `rows`".to_string())?;
            table_result_from_parts(columns, rows)
        }
        Value::List(mut pair) if is_table_result_pair(&pair) => {
            let rows = pair.pop().expect("table result pair has two values");
            let columns = pair.pop().expect("table result pair has two values");
            table_result_from_parts(columns, rows)
        }
        rows @ Value::List(_) => table_rows(rows, Vec::new()),
        _ => Err(
            "table callback must return { columns, rows }, [columns, rows], or row objects"
                .to_string(),
        ),
    }
}

fn is_table_result_pair(values: &[Value]) -> bool {
    values.len() == 2
        && matches!(&values[0], Value::List(columns) if columns.iter().all(|value| matches!(value, Value::Str(_) | Value::FixedChar(_))))
        && matches!(&values[1], Value::List(_))
}

fn table_result_from_parts(columns: Value, rows: Value) -> Result<SQLTableFunctionResult, String> {
    let Value::List(columns) = columns else {
        return Err("table result `columns` must be an array of strings".to_string());
    };
    let columns = columns
        .into_iter()
        .map(|column| match column {
            Value::Str(column) | Value::FixedChar(column) => Ok(column),
            _ => Err("table result `columns` must contain only strings".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    table_rows(rows, columns)
}

fn table_rows(rows: Value, mut columns: Vec<String>) -> Result<SQLTableFunctionResult, String> {
    let Value::List(rows) = rows else {
        return Err("table result `rows` must be an array".to_string());
    };
    let mut converted = Vec::with_capacity(rows.len());
    for row in rows {
        match row {
            Value::Map(values) => {
                if columns.is_empty() {
                    columns.extend(values.keys().cloned());
                }
                converted.push(
                    columns
                        .iter()
                        .map(|column| values.get(column).cloned().unwrap_or(Value::Null))
                        .collect(),
                );
            }
            Value::List(values) => {
                if columns.is_empty() {
                    return Err("table callback row arrays require explicit `columns`".to_string());
                }
                if values.len() != columns.len() {
                    return Err(format!(
                        "table callback row has {} values but {} columns",
                        values.len(),
                        columns.len()
                    ));
                }
                converted.push(values);
            }
            _ => return Err("each table callback row must be an object or array".to_string()),
        }
    }
    Ok(SQLTableFunctionResult::new(columns, converted))
}

pub(super) struct WASMAggregateFunction {
    pub(super) name: String,
    pub(super) callback_id: u32,
}

impl SQLAggregateFunction for WASMAggregateFunction {
    fn create_state(&self) -> Box<dyn SQLAggregateState> {
        match invoke(self.callback_id, &json!({ "operation": "aggregateCreate" })).and_then(
            |value| {
                let state_id = value
                    .as_u64()
                    .ok_or("aggregate factory returned an invalid state ID")?;
                u32::try_from(state_id)
                    .map_err(|_| "aggregate state ID exceeds the u32 range".to_string())
            },
        ) {
            Ok(state_id) => Box::new(WASMAggregateState {
                name: self.name.clone(),
                callback_id: self.callback_id,
                state_id: Some(state_id),
                finished: AtomicBool::new(false),
                init_error: None,
            }),
            Err(error) => Box::new(WASMAggregateState {
                name: self.name.clone(),
                callback_id: self.callback_id,
                state_id: None,
                finished: AtomicBool::new(false),
                init_error: Some(error),
            }),
        }
    }
}

struct WASMAggregateState {
    name: String,
    callback_id: u32,
    state_id: Option<u32>,
    finished: AtomicBool,
    init_error: Option<String>,
}

impl SQLAggregateState for WASMAggregateState {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        if let Some(error) = self.init_error.take() {
            return Err(callback_error(&self.name, "create aggregate state", error));
        }
        let state_id = self.state_id.ok_or_else(|| {
            callback_error(&self.name, "observe aggregate value", "state is missing")
        })?;
        let args = args
            .iter()
            .cloned()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| callback_error(&self.name, "prepare aggregate arguments", error))?;
        invoke(
            self.callback_id,
            &json!({
                "operation": "aggregateObserve",
                "stateId": state_id,
                "args": args,
            }),
        )
        .map(|_| ())
        .map_err(|error| callback_error(&self.name, "observe aggregate value", error))
    }

    fn finish(&self) -> Result<Value, SQLError> {
        if let Some(error) = self.init_error.as_ref() {
            return Err(callback_error(&self.name, "create aggregate state", error));
        }
        let state_id = self
            .state_id
            .ok_or_else(|| callback_error(&self.name, "finish aggregate", "state is missing"))?;
        if self.finished.swap(true, Ordering::AcqRel) {
            return Err(callback_error(
                &self.name,
                "finish aggregate",
                "state was already finished",
            ));
        }
        let value = invoke(
            self.callback_id,
            &json!({ "operation": "aggregateFinish", "stateId": state_id }),
        )
        .map_err(|error| callback_error(&self.name, "finish aggregate", error))?;
        value_from_json(&value)
            .map_err(|error| callback_error(&self.name, "convert aggregate result", error))
    }
}

impl Drop for WASMAggregateState {
    fn drop(&mut self) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        if let Some(state_id) = self.state_id {
            let _ = invoke(
                self.callback_id,
                &json!({ "operation": "aggregateDrop", "stateId": state_id }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_options_validate_types_and_defaults() {
        assert_eq!(
            function_options(&json!({})).unwrap(),
            SQLFunctionOptions::default()
        );
        assert_eq!(
            function_options(&json!({
                "options": { "volatility": "immutable", "mayMutateEngine": false }
            }))
            .unwrap(),
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable)
        );
        assert!(function_options(&json!({ "options": { "volatility": "sometimes" } })).is_err());
        assert!(function_options(&json!({ "options": { "mayMutateEngine": 1 } })).is_err());
    }

    #[test]
    fn table_results_accept_objects_and_validate_row_width() {
        let result = table_function_result(Value::List(vec![Value::Map(
            [("value".to_string(), Value::Int(3))].into_iter().collect(),
        )]))
        .unwrap();
        assert_eq!(result.columns, vec!["value"]);
        assert_eq!(result.rows, vec![vec![Value::Int(3)]]);

        let invalid = table_function_result(Value::List(vec![
            Value::List(vec![Value::Str("a".to_string())]),
            Value::List(vec![Value::List(vec![Value::Int(1), Value::Int(2)])]),
        ]));
        assert!(invalid.is_err());
    }
}
