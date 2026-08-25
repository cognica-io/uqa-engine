//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine overload resolution, argument binding, and coercion.

use super::{
    canonical_routine_type_name, cast_value, eval_lowered_expression, value_type_name, Arc,
    ArrayValue, CreateFunction, Engine, FunctionBinding, SQLError, SQLUserFunction, Value,
};
use crate::engine_user_functions::RoutineCallKind;
use uqa_sql::ast::{ColumnType, FunctionParamMode, RoutineInvocationBinding, RoutineVariadicMode};
use uqa_sql::expr::coercion_type_name;

pub(super) fn output_column_names(def: &CreateFunction) -> Vec<String> {
    def.output_params()
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            if p.name.is_empty() {
                format!("column{}", idx + 1)
            } else {
                p.name.clone()
            }
        })
        .collect()
}

pub(super) fn call_signature(name: &str, args: &[(Option<String>, Value)]) -> String {
    let types = args
        .iter()
        .map(|(arg_name, value)| match arg_name {
            Some(arg_name) => format!("{arg_name} => {}", value_type_name(value)),
            None => value_type_name(value).to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({types})")
}

pub(super) fn routine_resolution_error(
    kind: &str,
    name: &str,
    args: &[(Option<String>, Value)],
    suffix: &str,
) -> SQLError {
    SQLError::Routine {
        sqlstate: if suffix == "is not unique" {
            "42725".into()
        } else {
            "42883".into()
        },
        message: format!("{kind} {} {suffix}", call_signature(name, args)),
    }
}

/// A resolved overload plus its bound argument values.
pub(super) struct ResolvedRoutine {
    pub(super) function: Arc<SQLUserFunction>,
    pub(super) bound: Vec<Value>,
    pub(super) invocation: Box<RoutineInvocationBinding>,
}

/// Resolve `name(args)` to a single overload and its bound argument
/// values (declared-type casts applied, defaults evaluated).
/// `Ok(None)` = no routine with this name at all.
pub(super) fn resolve_routine(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
    declared_argument_types: Option<&[Option<ColumnType>]>,
    kind: &str,
    explicit_variadic: bool,
) -> Result<Option<ResolvedRoutine>, SQLError> {
    if engine.lookup_sql_functions(name).is_none() {
        return Ok(None);
    }
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let inferred_argument_types: Vec<Option<ColumnType>>;
    let argument_types = match declared_argument_types {
        Some(types) if types.len() == args.len() => types,
        Some(types) => {
            return Err(SQLError::Internal(format!(
                "routine argument type count {} does not match value count {}",
                types.len(),
                args.len()
            )));
        }
        None => {
            inferred_argument_types = runtime_argument_types(args)?;
            &inferred_argument_types
        }
    };
    let call_kind = if kind == "procedure" {
        RoutineCallKind::Procedure
    } else {
        RoutineCallKind::Function
    };
    let matched = engine
        .resolve_static_sql_routine_match(
            name,
            None,
            &argument_names,
            argument_types,
            explicit_variadic,
            call_kind,
        )?
        .ok_or_else(|| routine_resolution_error(kind, name, args, "does not exist"))?;
    let bound = materialize_arguments(engine, &matched.function.def, &matched.invocation, args)?;
    Ok(Some(ResolvedRoutine {
        function: matched.function,
        bound,
        invocation: matched.invocation,
    }))
}

pub(super) fn resolve_bound_routine(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Result<Option<ResolvedRoutine>, SQLError> {
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let argument_types = runtime_argument_types(args)?;
    let explicit_variadic = binding.invocation.as_ref().is_some_and(|invocation| {
        matches!(
            invocation.variadic_mode,
            RoutineVariadicMode::Explicit { .. }
        )
    });
    let matched = engine
        .resolve_static_sql_routine_match(
            &binding.name,
            Some(binding),
            &argument_names,
            &argument_types,
            explicit_variadic,
            RoutineCallKind::Function,
        )?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "bound function {}({}) does not exist",
                binding.name,
                binding.argument_types.join(", ")
            ),
        })?;
    let bound = materialize_arguments(engine, &matched.function.def, &matched.invocation, args)?;
    Ok(Some(ResolvedRoutine {
        function: matched.function,
        bound,
        invocation: matched.invocation,
    }))
}

fn runtime_argument_types(
    args: &[(Option<String>, Value)],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    args.iter()
        .map(|(_, value)| {
            if matches!(value, Value::Null) {
                Ok(None)
            } else {
                ColumnType::from_sql_name(value_type_name(value)).map(Some)
            }
        })
        .collect()
}

/// Evaluate defaults and apply declared-type casts for the winning
/// overload.
fn materialize_arguments(
    engine: &Engine,
    def: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    args: &[(Option<String>, Value)],
) -> Result<Vec<Value>, SQLError> {
    if invocation.argument_positions.len() != args.len()
        || invocation.argument_targets.len() != args.len()
        || invocation.parameter_types.len() != def.params.len()
    {
        return Err(SQLError::Internal(format!(
            "routine `{}` has an inconsistent invocation binding",
            def.name
        )));
    }
    let expanded_parameter = match invocation.variadic_mode {
        RoutineVariadicMode::Expanded { parameter_index } => Some(parameter_index),
        RoutineVariadicMode::None | RoutineVariadicMode::Explicit { .. } => None,
    };
    let mut slots = vec![None; def.params.len()];
    let mut expanded_values = Vec::new();
    for (argument_index, ((_, value), parameter_index)) in
        args.iter().zip(&invocation.argument_positions).enumerate()
    {
        let target = &invocation.argument_targets[argument_index];
        let value = coerce_routine_value(engine, value, target)?;
        if Some(*parameter_index) == expanded_parameter {
            expanded_values.push(value);
        } else if slots[*parameter_index].replace(value).is_some() {
            return Err(SQLError::Internal(format!(
                "routine `{}` bound more than one argument to parameter {}",
                def.name,
                parameter_index + 1
            )));
        }
    }
    if let Some(parameter_index) = expanded_parameter {
        let array = ArrayValue::try_new(expanded_values).ok_or_else(|| {
            SQLError::Internal(format!(
                "routine `{}` could not materialize its variadic array",
                def.name
            ))
        })?;
        slots[parameter_index] = Some(coerce_routine_value(
            engine,
            &Value::Array(array),
            &invocation.parameter_types[parameter_index],
        )?);
    }
    let mut bound = Vec::with_capacity(def.call_arity());
    for (parameter_index, parameter) in def.params.iter().enumerate() {
        let takes_argument = match parameter.mode {
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic => true,
            FunctionParamMode::Out => def.is_procedure,
            FunctionParamMode::Table => false,
        };
        if !takes_argument {
            continue;
        }
        let value = if let Some(value) = slots[parameter_index].take() {
            value
        } else {
            let default = parameter.default.as_ref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "routine `{}` lost required parameter {} after overload resolution",
                    def.name,
                    parameter_index + 1
                ))
            })?;
            let value = eval_lowered_expression(engine, default, None, &[])?;
            coerce_routine_value(engine, &value, &invocation.parameter_types[parameter_index])?
        };
        bound.push(value);
    }
    Ok(bound)
}

/// Apply a routine declaration's already-resolved SQL type. Pseudo-types use
/// their own carrier validation; every scalar type goes through the SQL cast
/// layer and an unsupported declaration remains an error.
pub(super) fn coerce_routine_value(
    engine: &Engine,
    value: &Value,
    type_name: &str,
) -> Result<Value, SQLError> {
    match canonical_routine_type_name(type_name).as_str() {
        "record" => match value {
            Value::Record(_) | Value::Null => Ok(value.clone()),
            Value::Row(values) => Ok(Value::Record(
                values
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| (format!("f{}", index + 1), value))
                    .collect(),
            )),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast non-composite value to type record".into(),
            }),
        },
        "trigger" => match value {
            Value::Record(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "trigger function must return a row or NULL".into(),
            }),
        },
        "anyarray" => match value {
            Value::Array(_) | Value::List(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast non-array value to type anyarray".into(),
            }),
        },
        "refcursor" => match value {
            Value::Str(_) | Value::Null => Ok(value.clone()),
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot cast value to type refcursor".into(),
            }),
        },
        "void" if matches!(value, Value::Null) => Ok(Value::Null),
        "void" => Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "cannot cast non-null value to type void".into(),
        }),
        _ => {
            let target = crate::sql::resolve_catalog_column_type(engine, type_name)
                .as_ref()
                .map_or_else(|| type_name.to_string(), coercion_type_name);
            cast_value(value, &target)
        }
    }
}
