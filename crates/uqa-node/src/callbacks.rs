//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JavaScript adapters for the engine SQL callback contracts.

use std::sync::mpsc;
use std::thread::{self, ThreadId};

use napi::bindgen_prelude::{
    Env, FromNapiValue, Function, FunctionRef, JsValue, JsValuesTupleIntoVec, Object, Result,
    Status, TypeName, Unknown, ValidateNapiValue, ValueType,
};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{sys, Error};
use napi_derive::napi;
use uqa_core::Value;
use uqa_engine::{
    SQLAggregateFunction, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility,
    SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult,
};
use uqa_sql::SQLError;

use crate::value::{value_from_unknown, value_to_napi};

#[napi(string_enum = "lowercase")]
pub enum JSFunctionVolatility {
    Volatile,
    Stable,
    Immutable,
}

#[napi(object, js_name = "SQLFunctionOptions")]
pub struct JSFunctionOptions {
    #[napi(ts_type = "\"volatile\" | \"stable\" | \"immutable\"")]
    pub volatility: Option<JSFunctionVolatility>,
    pub may_mutate_engine: Option<bool>,
}

pub(super) fn function_options(options: Option<JSFunctionOptions>) -> SQLFunctionOptions {
    let Some(options) = options else {
        return SQLFunctionOptions::default();
    };
    let volatility = match options.volatility.unwrap_or(JSFunctionVolatility::Volatile) {
        JSFunctionVolatility::Volatile => SQLFunctionVolatility::Volatile,
        JSFunctionVolatility::Stable => SQLFunctionVolatility::Stable,
        JSFunctionVolatility::Immutable => SQLFunctionVolatility::Immutable,
    };
    SQLFunctionOptions::new(volatility, options.may_mutate_engine.unwrap_or(true))
}

pub struct CallbackArguments(Vec<Value>);

impl JsValuesTupleIntoVec for CallbackArguments {
    fn into_vec(self, env: sys::napi_env) -> Result<Vec<sys::napi_value>> {
        self.0
            .into_iter()
            .map(|value| unsafe { value_to_napi(env, value) })
            .collect()
    }
}

pub struct SynchronousJSValue(Value);

impl TypeName for SynchronousJSValue {
    fn type_name() -> &'static str {
        "synchronous JavaScript value"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ValidateNapiValue for SynchronousJSValue {}

impl FromNapiValue for SynchronousJSValue {
    unsafe fn from_napi_value(env: sys::napi_env, napi_value: sys::napi_value) -> Result<Self> {
        let value = unsafe { Unknown::from_raw_unchecked(env, napi_value) };
        if value.is_promise()? {
            return Err(Error::from_reason(
                "SQL callbacks must return synchronously; Promise results are not supported",
            ));
        }
        Ok(Self(value_from_unknown(&value)?))
    }
}

type CallbackThreadsafeFunction<Return> =
    ThreadsafeFunction<Vec<Value>, Return, CallbackArguments, Status, false, true, 0>;

struct JSFunction<Return>
where
    Return: FromNapiValue + Send + 'static,
{
    owner_thread: ThreadId,
    env: usize,
    function: FunctionRef<CallbackArguments, Return>,
    threadsafe: CallbackThreadsafeFunction<Return>,
}

impl<Return> JSFunction<Return>
where
    Return: FromNapiValue + Send + 'static,
{
    fn new(function: Function<'_, CallbackArguments, Return>) -> Result<Self> {
        let env = function.value().env;
        let reference = function.create_ref()?;
        let threadsafe = function
            .build_threadsafe_function::<Vec<Value>>()
            .weak::<true>()
            .build_callback(|context| Ok(CallbackArguments(context.value)))?;
        Ok(Self {
            owner_thread: thread::current().id(),
            env: env.cast::<()>() as usize,
            function: reference,
            threadsafe,
        })
    }

    fn call(&self, args: &[Value]) -> std::result::Result<Return, String> {
        if thread::current().id() == self.owner_thread {
            let env = Env::from_raw(self.env as sys::napi_env);
            let function = self.function.borrow_back(&env).map_err(napi_error)?;
            return function
                .call(CallbackArguments(args.to_vec()))
                .map_err(napi_error);
        }

        let (sender, receiver) = mpsc::sync_channel(1);
        let status = self.threadsafe.call_with_return_value(
            args.to_vec(),
            ThreadsafeFunctionCallMode::Blocking,
            move |result, _env| {
                let result = result.map_err(napi_error);
                sender
                    .send(result)
                    .map_err(|_| Error::from_reason("SQL callback result receiver was dropped"))
            },
        );
        if status != Status::Ok {
            return Err(format!("could not schedule JavaScript callback: {status}"));
        }
        receiver
            .recv()
            .map_err(|_| "JavaScript callback result channel closed unexpectedly".to_string())?
    }
}

fn napi_error(error: Error) -> String {
    error.to_string()
}

fn callback_error(name: &str, operation: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!(
        "JavaScript SQL function `{name}` failed to {operation}: {error}"
    ))
}

pub(super) struct JSScalarFunction {
    pub(super) name: String,
    callback: JSFunction<SynchronousJSValue>,
}

impl JSScalarFunction {
    pub(super) fn new(
        name: String,
        callback: Function<'_, CallbackArguments, SynchronousJSValue>,
    ) -> Result<Self> {
        Ok(Self {
            name,
            callback: JSFunction::new(callback)?,
        })
    }
}

impl SQLScalarFunction for JSScalarFunction {
    fn call(&self, args: &[Value]) -> std::result::Result<Value, SQLError> {
        self.callback
            .call(args)
            .map(|value| value.0)
            .map_err(|error| callback_error(&self.name, "call scalar callback", error))
    }
}

pub(super) struct JSTableFunction {
    pub(super) name: String,
    callback: JSFunction<SynchronousJSValue>,
}

impl JSTableFunction {
    pub(super) fn new(
        name: String,
        callback: Function<'_, CallbackArguments, SynchronousJSValue>,
    ) -> Result<Self> {
        Ok(Self {
            name,
            callback: JSFunction::new(callback)?,
        })
    }
}

impl SQLTableFunction for JSTableFunction {
    fn call(&self, args: &[Value]) -> std::result::Result<SQLTableFunctionResult, SQLError> {
        let value = self
            .callback
            .call(args)
            .map_err(|error| callback_error(&self.name, "call table callback", error))?
            .0;
        SQLTableFunctionResult::from_dynamic_value(value)
            .map_err(|error| callback_error(&self.name, "convert table result", error))
    }
}

pub struct JSAggregateCallbacks {
    observe: JSFunction<SynchronousJSValue>,
    finish: JSFunction<SynchronousJSValue>,
}

impl TypeName for JSAggregateCallbacks {
    fn type_name() -> &'static str {
        "SQL aggregate state"
    }

    fn value_type() -> ValueType {
        ValueType::Object
    }
}

impl ValidateNapiValue for JSAggregateCallbacks {}

impl FromNapiValue for JSAggregateCallbacks {
    unsafe fn from_napi_value(env: sys::napi_env, napi_value: sys::napi_value) -> Result<Self> {
        let value = unsafe { Unknown::from_raw_unchecked(env, napi_value) };
        if value.is_promise()? {
            return Err(Error::from_reason(
                "SQL aggregate factories must return synchronously; Promise results are not supported",
            ));
        }
        if value.get_type()? != ValueType::Object {
            return Err(Error::from_reason(
                "SQL aggregate factory must return an object",
            ));
        }
        let object = unsafe { value.cast::<Object>() }?;
        let observe = aggregate_method(&object, "observe", "step")?;
        let finish = aggregate_method(&object, "finish", "finalize")?;
        Ok(Self {
            observe: JSFunction::new(observe.bind(object)?)?,
            finish: JSFunction::new(finish.bind(object)?)?,
        })
    }
}

fn aggregate_method<'env>(
    object: &'env Object<'env>,
    preferred: &str,
    fallback: &str,
) -> Result<Function<'env, CallbackArguments, SynchronousJSValue>> {
    if let Some(method) = object.get(preferred)? {
        return Ok(method);
    }
    object.get(fallback)?.ok_or_else(|| {
        Error::from_reason(format!(
            "SQL aggregate state needs a `{preferred}` or `{fallback}` method"
        ))
    })
}

pub(super) struct JSAggregateFunction {
    pub(super) name: String,
    factory: JSFunction<JSAggregateCallbacks>,
}

impl JSAggregateFunction {
    pub(super) fn new(
        name: String,
        factory: Function<'_, CallbackArguments, JSAggregateCallbacks>,
    ) -> Result<Self> {
        Ok(Self {
            name,
            factory: JSFunction::new(factory)?,
        })
    }
}

impl SQLAggregateFunction for JSAggregateFunction {
    fn create_state(&self) -> Box<dyn SQLAggregateState> {
        match self.factory.call(&[]) {
            Ok(callbacks) => Box::new(JSAggregateState {
                name: self.name.clone(),
                callbacks: Some(callbacks),
                init_error: None,
            }),
            Err(error) => Box::new(JSAggregateState {
                name: self.name.clone(),
                callbacks: None,
                init_error: Some(error),
            }),
        }
    }
}

struct JSAggregateState {
    name: String,
    callbacks: Option<JSAggregateCallbacks>,
    init_error: Option<String>,
}

impl SQLAggregateState for JSAggregateState {
    fn observe(&mut self, args: &[Value]) -> std::result::Result<(), SQLError> {
        if let Some(error) = self.init_error.take() {
            return Err(callback_error(&self.name, "create aggregate state", error));
        }
        let callbacks = self.callbacks.as_ref().ok_or_else(|| {
            callback_error(&self.name, "observe aggregate value", "state is missing")
        })?;
        callbacks
            .observe
            .call(args)
            .map(|_| ())
            .map_err(|error| callback_error(&self.name, "observe aggregate value", error))
    }

    fn finish(&self) -> std::result::Result<Value, SQLError> {
        if let Some(error) = self.init_error.as_ref() {
            return Err(callback_error(&self.name, "create aggregate state", error));
        }
        let callbacks = self
            .callbacks
            .as_ref()
            .ok_or_else(|| callback_error(&self.name, "finish aggregate", "state is missing"))?;
        callbacks
            .finish
            .call(&[])
            .map(|value| value.0)
            .map_err(|error| callback_error(&self.name, "finish aggregate", error))
    }
}
