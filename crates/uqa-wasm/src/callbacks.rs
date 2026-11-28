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

use super::{value_from_json, wire::WireValues};

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
        Some(JSON::String(value)) => value.parse()?,
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
    invoke_text(callback_id, request.to_string())
}

#[derive(serde::Serialize)]
struct CallbackArguments<'a> {
    args: WireValues<'a>,
    operation: &'static str,
    #[serde(rename = "stateId", skip_serializing_if = "Option::is_none")]
    state_id: Option<u32>,
}

fn invoke_text(callback_id: u32, request: String) -> Result<JSON, String> {
    let request = CString::new(request)
        .map_err(|_| "SQL callback request contains an interior NUL".to_string())?;
    let text = invoke_raw(callback_id, request.as_ptr())?;
    let mut response: JSON = serde_json::from_str(&text)
        .map_err(|error| format!("SQL callback returned invalid JSON: {error}"))?;
    if let Some(error) = response.get("error") {
        return Err(error
            .as_str()
            .map_or_else(|| error.to_string(), ToString::to_string));
    }
    response
        .get_mut("ok")
        .map(JSON::take)
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
        let request = serde_json::to_string(&CallbackArguments {
            args: WireValues(args),
            operation: "scalar",
            state_id: None,
        })
        .map_err(|error| callback_error(&self.name, "prepare scalar arguments", error))?;
        let result = invoke_text(self.callback_id, request)
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
        let request = serde_json::to_string(&CallbackArguments {
            args: WireValues(args),
            operation: "table",
            state_id: None,
        })
        .map_err(|error| callback_error(&self.name, "prepare table arguments", error))?;
        let result = invoke_text(self.callback_id, request)
            .map_err(|error| callback_error(&self.name, "call table callback", error))?;
        let value = value_from_json(&result)
            .map_err(|error| callback_error(&self.name, "decode table result", error))?;
        SQLTableFunctionResult::from_dynamic_value(value)
            .map_err(|error| callback_error(&self.name, "convert table result", error))
    }
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
        let request = serde_json::to_string(&CallbackArguments {
            args: WireValues(args),
            operation: "aggregateObserve",
            state_id: Some(state_id),
        })
        .map_err(|error| callback_error(&self.name, "prepare aggregate arguments", error))?;
        invoke_text(self.callback_id, request)
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
}
