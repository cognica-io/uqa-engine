//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stateful sequence dispatch and built-in function-family routing.

use super::{to_i64, value_to_string_with_control, EvalContext, Result, SQLError, Value};

use uqa_core::memory::{Produced, ProductionControl};

type ScalarFunctionDispatcher =
    fn(&str, &[Value], Option<&dyn super::SQLValueCatalog>) -> Option<Result<Value>>;
type RetainedScalarDispatcher =
    fn(&str, &[Value], &ProductionControl<'_>) -> Option<Result<Produced<Value>>>;
struct ScalarFamily {
    ordinary: ScalarFunctionDispatcher,
    retained: RetainedScalarDispatcher,
}
const FAMILIES: &[ScalarFamily] = &[
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_core::eval_core_functions(name, args),
        retained: super::scalar_core::eval_core_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_math::eval_math_functions(name, args),
        retained: super::scalar_math::eval_math_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_temporal::eval_temporal_functions(name, args),
        retained: super::scalar_temporal::eval_temporal_functions_with_control,
    },
    ScalarFamily {
        ordinary: super::scalar_json::eval_json_functions,
        retained: super::scalar_json::eval_json_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_array::eval_array_functions(name, args),
        retained: super::scalar_array::eval_array_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_postgres::eval_postgres_functions(name, args),
        retained: super::scalar_postgres::eval_postgres_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_range::eval_range_functions(name, args),
        retained: super::scalar_range::eval_range_functions_with_control,
    },
    ScalarFamily {
        ordinary: |name, args, _| super::scalar_geospatial::eval_geospatial_functions(name, args),
        retained: super::scalar_geospatial::eval_geospatial_functions_with_control,
    },
];

// -------------------------------------------------------------------------
// Built-in scalar functions
// -------------------------------------------------------------------------

/// Dispatch table for built-in scalar SQL functions. Function
/// names are lower-cased before lookup.
pub(super) fn eval_sequence_function(
    name: &str,
    args: &[Value],
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    eval_sequence_function_with_control(name, args, ctx, &ProductionControl::uncontrolled())
        .map(|value| value.into_uncontrolled().expect("ordinary sequence result"))
}

pub(super) fn eval_sequence_function_with_control(
    name: &str,
    args: &[Value],
    ctx: &EvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported(format!(
            "sequence function `{name}` requires an engine hook on the EvalContext"
        ))
    })?;
    let (valid_arity, expected) = match name {
        "nextval" | "currval" => (args.len() == 1, "1"),
        "lastval" => (args.is_empty(), "0"),
        "setval" => (matches!(args.len(), 2 | 3), "2 or 3"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "unknown sequence function `{other}`"
            )));
        }
    };
    if !valid_arity {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: expected.into(),
            actual: args.len(),
        });
    }
    if args.iter().any(|argument| matches!(argument, Value::Null)) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let value = match name {
        "nextval" => with_sequence_name(&args[0], control, |name| engine.nextval(name)),
        "currval" => with_sequence_name(&args[0], control, |name| engine.currval(name)),
        "lastval" => engine.lastval(),
        "setval" => with_sequence_name(&args[0], control, |seq_name| {
            let n = to_i64(&args[1])?;
            let is_called = match args.get(2) {
                None => true,
                Some(Value::Bool(is_called)) => *is_called,
                Some(value) => {
                    return Err(SQLError::TypeMismatch(format!(
                        "setval requires a boolean third argument, got {value:?}"
                    )));
                }
            };
            engine.setval(seq_name, n, is_called)
        }),
        _ => unreachable!("sequence function name was validated above"),
    }?;
    Ok(control.finish(Value::Int(value), control.empty_reservation())?)
}

fn with_sequence_name<T>(
    value: &Value,
    control: &ProductionControl<'_>,
    read: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    control.check()?;
    if let Value::Str(name) = value {
        read(name)
    } else {
        let name = value_to_string_with_control(value, control)?;
        read(&name)
    }
}

/// `collation` is the call's input collation (`PostgreSQL`'s `inputcollid`), which collation-dependent built-ins consume.
#[cfg(test)]
pub(super) fn eval_scalar_function(name: &str, args: &[Value], collation: u32) -> Result<Value> {
    eval_scalar_function_with_catalog(name, args, collation, None)
}

fn eval_scalar_function_with_catalog(
    name: &str,
    args: &[Value],
    collation: u32,
    catalog: Option<&dyn super::SQLValueCatalog>,
) -> Result<Value> {
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if super::builtin_scalar_function_strictness(name, args.len()) == Some(true)
        && args.iter().any(|argument| matches!(argument, Value::Null))
    {
        return Ok(Value::Null);
    }
    if let Some(result) = super::scalar_core::eval_collated_functions_with_control(
        name,
        args,
        collation,
        &ProductionControl::uncontrolled(),
    ) {
        return result.map(|value| value.into_uncontrolled().expect("ordinary collated result"));
    }
    for family in FAMILIES {
        if let Some(result) = (family.ordinary)(name, args, catalog) {
            return result;
        }
    }
    Err(SQLError::UnknownFunction(name.to_string()))
}

pub(super) fn eval_scalar_function_with_context(
    name: &str,
    args: &[Value],
    context: &EvalContext<'_>,
    collation: u32,
) -> Result<Value> {
    if name.strip_prefix("pg_catalog.").unwrap_or(name) == "uuidv7" {
        return super::uuid::eval_uuid_v7(args, Some(context));
    }
    if name.strip_prefix("pg_catalog.").unwrap_or(name) == "timezone" {
        return super::time::eval_timezone(args, Some(context), &ProductionControl::uncontrolled())
            .map(|value| value.into_uncontrolled().expect("ordinary temporal result"));
    }
    if let Some(result) = eval_array_comparison_with_context(
        name.strip_prefix("pg_catalog.").unwrap_or(name),
        args,
        &ProductionControl::uncontrolled(),
        context,
    ) {
        return result.map(|value| value.into_uncontrolled().expect("ordinary array result"));
    }
    let catalog = context.engine.map(super::EngineValueCatalog);
    eval_scalar_function_with_catalog(
        name,
        args,
        collation,
        catalog
            .as_ref()
            .map(|catalog| catalog as &dyn super::SQLValueCatalog),
    )
}

fn eval_array_comparison_with_context(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Option<Result<Produced<Value>>> {
    if !matches!(
        name,
        "array_sort" | "array_remove" | "array_position" | "array_positions" | "array_replace"
    ) {
        return None;
    }
    let catalog = context.engine.map(super::EngineValueCatalog);
    let catalog = catalog
        .as_ref()
        .map(|catalog| catalog as &dyn super::SQLValueCatalog);
    if matches!(name, "array_sort" | "array_remove" | "array_position") {
        super::scalar_array::eval_array_functions_with_catalog(name, args, control, catalog)
    } else {
        super::scalar_postgres::eval_postgres_arrays_with_catalog(name, args, control, catalog)
    }
}

pub(super) fn eval_generated_scalar_function_with_context(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
    collation: u32,
) -> Result<Produced<Value>> {
    if name.strip_prefix("pg_catalog.").unwrap_or(name) == "timezone" {
        return super::time::eval_timezone(args, Some(context), control);
    }
    if let Some(result) = eval_array_comparison_with_context(
        name.strip_prefix("pg_catalog.").unwrap_or(name),
        args,
        control,
        context,
    ) {
        return result;
    }
    eval_generated_scalar_function(name, args, control, collation)
}

/// Built-in families accepted by generated expressions use their existing admitted kernels in the same dispatch order. Stateful dispatch remains outside this interface.
pub(super) fn eval_generated_scalar_function(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
    collation: u32,
) -> Result<Produced<Value>> {
    control.check()?;
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if super::builtin_scalar_function_strictness(name, args.len()) == Some(true)
        && args.iter().any(|argument| matches!(argument, Value::Null))
    {
        return control
            .finish(Value::Null, control.empty_reservation())
            .map_err(Into::into);
    }
    if let Some(result) =
        super::scalar_core::eval_collated_functions_with_control(name, args, collation, control)
    {
        return result;
    }
    for family in FAMILIES {
        if let Some(result) = (family.retained)(name, args, control) {
            return result;
        }
    }
    Err(SQLError::UnknownFunction(name.to_string()))
}
