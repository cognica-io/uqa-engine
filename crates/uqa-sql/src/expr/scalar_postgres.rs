//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extended `PostgreSQL` scalar and lowered operator built-ins.

use super::{out_of_range, to_i64, ArrayValue, Result, SQLError, Value};
use crate::ast::FunctionDispatch;

mod arrays;
mod comparison;
mod immutable;
pub(super) use immutable::{
    eval_postgres_immutable_with_control, eval_postgres_integer_base_with_control,
};
mod subscripts;
pub(super) use arrays::eval_postgres_arrays_with_control;
pub(super) use subscripts::eval_postgres_subscript_with_control;
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn eval_postgres_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if let Some(result) =
        eval_postgres_functions_with_control(name, args, &ProductionControl::uncontrolled())
    {
        return Some(result.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary PostgreSQL result")
        }));
    }
    const NAMES: &[&str] = &[
        "string_to_table",
        "current_database",
        "version",
        "current_catalog",
        "current_user",
        "session_user",
        "array_sample",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(eval_postgres_function(name, args))
}

pub(super) fn eval_postgres_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if let Some(result) = eval_postgres_immutable_with_control(name, args, control) {
        return Some(result);
    }
    if let Some(result) = eval_postgres_arrays_with_control(name, args, control) {
        return Some(result);
    }
    super::regex::evaluate(name, args, control)
}

pub(super) fn eval_dispatched_postgres_function(
    dispatch: FunctionDispatch,
    args: &[Value],
) -> Option<Result<Value>> {
    eval_dispatched_postgres_function_with_control(
        dispatch,
        args,
        &ProductionControl::uncontrolled(),
    )
    .map(|result| {
        result.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary dispatched PostgreSQL result")
        })
    })
}

pub(super) fn eval_dispatched_postgres_function_with_control(
    dispatch: FunctionDispatch,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if let Some(result) = eval_postgres_subscript_with_control(dispatch, args, control) {
        return Some(result);
    }
    if let Some(result) = eval_postgres_integer_base_with_control(dispatch, args, control) {
        return Some(result);
    }
    comparison::evaluate(dispatch, args, control)
}

fn eval_postgres_function(name: &str, args: &[Value]) -> Result<Value> {
    (|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // PostgreSQL scalar surface: math, strings, arrays, operators
            // lowered to internal functions.
            // -------------------------------------------------------------
            "string_to_table" => {
                immutable::string_to_array(args, &ProductionControl::uncontrolled()).map(|value| {
                    value
                        .into_uncontrolled()
                        .expect("ordinary string_to_table result")
                })
            }
            // The engine has one database and one logical user identity; schema
            // identifiers are intercepted above because they are session-scoped.
            "current_database" | "current_catalog" => Ok(Value::Str("uqa".into())),
            "version" => {
                if !args.is_empty() {
                    return Err(SQLError::BadArity {
                        name: name.into(),
                        expected: "0".into(),
                        actual: args.len(),
                    });
                }
                Ok(Value::Str(format!(
                    "UQA Engine {} on {}-{}, PostgreSQL 18 compatible",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::ARCH,
                    std::env::consts::OS,
                )))
            }
            "current_user" | "session_user" => Ok(Value::Str("uqa".into())),
            "array_sample" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_sample takes 2 args".into()));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("array_sample: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > array.elements().len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!(
                            "sample size must be between 0 and {}",
                            array.elements().len()
                        ),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array sample size"))?;
                let mut pool = array.elements().to_vec();
                let mut out = Vec::with_capacity(n);
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64 | 1)
                    .unwrap_or(1);
                for _ in 0..n {
                    seed = seed
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let idx = (seed >> 33) as usize % pool.len();
                    out.push(pool.swap_remove(idx));
                }
                let mut lower_bounds = array.lower_bounds().to_vec();
                if let Some(lower_bound) = lower_bounds.first_mut() {
                    *lower_bound = 1;
                }
                rebuild_array_with_bounds(out, lower_bounds)
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })()
}

fn rebuild_array_with_bounds(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Result<Value> {
    let rebuilt = if elements.is_empty() {
        ArrayValue::try_new(elements)
    } else {
        ArrayValue::with_lower_bounds(elements, lower_bounds)
    };
    rebuilt
        .map(Value::Array)
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions do not match".into()))
}
