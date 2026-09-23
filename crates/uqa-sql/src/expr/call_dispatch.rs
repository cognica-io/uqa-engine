//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar call dispatch after arguments have been normalized and evaluated.

use uqa_core::{ArrayValue, Value};

use crate::error::{Result, SQLError};

use super::call_arguments::normalized_function_name;
use super::context::EvalContext;
use super::conversion::to_f64;
use super::diagnostics::{unknown_function_error, value_type_name};
use super::json::{jsonpath_candidate, jsonpath_match};
use super::random;
use super::scalar_dispatch::{eval_scalar_function, eval_sequence_function};

mod named;
use named::builtin_named_args;

/// Execute a scalar function after its argument expressions have already been evaluated.
///
/// This is the shared SQL-semantics kernel used by both the parser AST evaluator and the physical scalar IR evaluator. Keeping dispatch here avoids converting a physical expression back into an AST [`crate::ast::Expr`] merely to reuse built-in, sequence, registered, or user-defined function behavior.
pub fn eval_function_call(
    name: &str,
    call_args: Vec<(Option<String>, Value)>,
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    eval_function_call_inner(name, call_args, ctx, true)
}

/// Execute a call whose stored binding selects a built-in routine. Dynamic
/// runtime callbacks and SQL routines must not override this stable binding.
pub fn eval_builtin_function_call(
    name: &str,
    call_args: Vec<(Option<String>, Value)>,
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    eval_function_call_inner(name, call_args, ctx, false)
}

#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
fn eval_function_call_inner(
    name: &str,
    call_args: Vec<(Option<String>, Value)>,
    ctx: &EvalContext<'_>,
    allow_dynamic_dispatch: bool,
) -> Result<Value> {
    let lower = normalized_function_name(name);
    let lower = lower.as_ref();
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();

    if let Some(result) = super::current_time::eval_current_time(lower, &evaluated, Some(ctx)) {
        return result;
    }

    if lower == "current_setting" {
        return super::session_settings::current_setting(&evaluated, ctx);
    }

    if let Some(result) = random::eval_random_function(lower, &call_args, ctx) {
        return result;
    }
    if lower == "random" && !evaluated.is_empty() {
        return Err(SQLError::TypeMismatch("random takes no arguments".into()));
    }
    if lower == "setseed" {
        let [value] = evaluated.as_slice() else {
            return Err(SQLError::TypeMismatch("setseed takes 1 arg".into()));
        };
        let seed = to_f64(value)?;
        if !seed.is_finite() || !(-1.0..=1.0).contains(&seed) {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("setseed parameter {seed} is out of allowed range [-1,1]"),
            });
        }
        let engine = ctx.engine.ok_or_else(|| {
            SQLError::Unsupported("setseed requires a logical engine session".into())
        })?;
        if !engine.set_random_seed(seed).map_err(SQLError::Internal)? {
            return Err(SQLError::Unsupported(
                "engine hook does not provide a session random stream".into(),
            ));
        }
        return Ok(Value::Str(String::new()));
    }

    if lower == "current_schema" {
        if !evaluated.is_empty() {
            return Err(SQLError::TypeMismatch(
                "current_schema takes no arguments".into(),
            ));
        }
        let schema = ctx
            .engine
            .map(|engine| engine.current_schema())
            .transpose()
            .map_err(SQLError::Internal)?
            .flatten()
            .unwrap_or_else(|| "public".to_string());
        return Ok(Value::Str(schema));
    }
    if lower == "current_schemas" {
        let [Value::Bool(include_implicit)] = evaluated.as_slice() else {
            return Err(SQLError::TypeMismatch(
                "current_schemas takes one boolean argument".into(),
            ));
        };
        let schemas = ctx
            .engine
            .map(|engine| engine.current_schemas(*include_implicit))
            .transpose()
            .map_err(SQLError::Internal)?
            .flatten()
            .unwrap_or_else(|| {
                let mut schemas = Vec::new();
                if *include_implicit {
                    schemas.push("pg_catalog".to_string());
                }
                schemas.push("public".to_string());
                schemas
            });
        return ArrayValue::try_new(schemas.into_iter().map(Value::Str).collect())
            .map(Value::Array)
            .ok_or_else(|| SQLError::TypeMismatch("invalid current_schemas result".into()));
    }
    if matches!(lower, "current_user" | "session_user") {
        if !evaluated.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "{lower} takes no arguments"
            )));
        }
        let user = ctx
            .engine
            .map(|engine| {
                if lower == "current_user" {
                    engine.current_user()
                } else {
                    engine.session_user()
                }
            })
            .transpose()?
            .flatten()
            .unwrap_or_else(|| "uqa".to_string());
        return Ok(Value::Str(user));
    }
    let regobject_type = match lower {
        "to_regproc" => Some(crate::ast::ColumnType::Regproc),
        "to_regprocedure" => Some(crate::ast::ColumnType::Regprocedure),
        "to_regclass" => Some(crate::ast::ColumnType::Regclass),
        "to_regnamespace" => Some(crate::ast::ColumnType::Regnamespace),
        "to_regrole" => Some(crate::ast::ColumnType::Regrole),
        "to_regtype" => Some(crate::ast::ColumnType::Regtype),
        _ => None,
    };
    if let Some(regobject_type) = regobject_type {
        let [value] = evaluated.as_slice() else {
            return Err(SQLError::BadArity {
                name: lower.into(),
                expected: "1".into(),
                actual: evaluated.len(),
            });
        };
        let name = match value {
            Value::Null => return Ok(Value::Null),
            Value::Str(name) | Value::FixedChar(name) => name,
            value => {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower} requires text, got {}",
                    value_type_name(value)
                )));
            }
        };
        let oid = ctx
            .engine
            .map(|engine| engine.resolve_regobject(&regobject_type, name))
            .transpose()?
            .flatten();
        return Ok(oid.map_or(Value::Null, Value::Int));
    }

    // Functions registered in the operator registry (text_match,
    // knn_match, ...) are dispatched by the relational/access-path
    // executor. JSONPath fts_match is the scalar exception.
    if crate::registry::is_registered(lower) {
        if lower == "fts_match" && jsonpath_candidate(&evaluated) {
            return jsonpath_match(&evaluated);
        }
        return Err(SQLError::Unsupported(format!(
            "scalar evaluation of `{name}` is not supported (use the function registry)"
        )));
    }

    if call_args.iter().any(|(name, _)| name.is_some()) {
        if let Some(positional) = builtin_named_args(
            lower,
            &call_args,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )? {
            return eval_scalar_function(lower, &positional);
        }
        if let Some(engine) = ctx.engine.filter(|_| allow_dynamic_dispatch) {
            if let Some(result) = engine.call_user_function(lower, &call_args) {
                return result;
            }
        }
        return Err(unknown_function_error(lower, &call_args));
    }

    // Sequence functions use engine-owned session state and therefore precede pure built-in dispatch.
    if matches!(lower, "nextval" | "currval" | "lastval" | "setval") {
        return eval_sequence_function(lower, &evaluated, ctx);
    }
    if let Some(engine) = ctx
        .engine
        .filter(|engine| allow_dynamic_dispatch && engine.has_scalar_functions())
    {
        if let Some(result) = engine.call_scalar_function(lower, &evaluated) {
            return result;
        }
    }
    match eval_scalar_function(lower, &evaluated) {
        // Unknown built-in: fall through to user-defined functions,
        // mirroring PostgreSQL's search-path order.
        Err(SQLError::UnknownFunction(_)) => {
            if let Some(engine) = ctx.engine.filter(|_| allow_dynamic_dispatch) {
                if let Some(result) = engine.call_user_function(lower, &call_args) {
                    return result;
                }
            }
            Err(unknown_function_error(lower, &call_args))
        }
        other => other,
    }
}
