//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine overload resolution, argument binding, and coercion.

use super::{
    canonical_routine_type_name, cast_value, eval_lowered_expression, value_type_name, Arc,
    CreateFunction, Engine, FunctionBinding, SQLError, SQLUserFunction, Value,
};
use crate::engine_user_functions::RoutineCallKind;
use uqa_execution::{match_function_signature, FunctionParameterDescriptor};
use uqa_sql::ast::ColumnType;
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

/// One argument slot after structural matching.
enum ArgSlot {
    Filled(Value),
    NeedsDefault(usize),
}

/// Match a call's argument list against a routine signature without
/// evaluating defaults. `None` = not a candidate.
fn try_match_arguments(
    def: &CreateFunction,
    args: &[(Option<String>, Value)],
) -> Option<Vec<ArgSlot>> {
    let signature = def.signature_params();
    let parameters = signature
        .iter()
        .map(|parameter| FunctionParameterDescriptor {
            name: Some(parameter.name.clone()),
            type_name: canonical_routine_type_name(&parameter.type_name),
            has_default: parameter.default.is_some(),
        })
        .collect::<Vec<_>>();
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let argument_types = vec![None; args.len()];
    let matched = match_function_signature(&parameters, &argument_names, &argument_types)?;
    if args.len() > signature.len() {
        return None;
    }
    let mut slots: Vec<Option<ArgSlot>> = (0..signature.len()).map(|_| None).collect();
    for ((_, value), position) in args.iter().zip(matched.argument_positions) {
        if slots[position].is_some() {
            return None;
        }
        slots[position] = Some(ArgSlot::Filled(value.clone()));
    }
    let mut out = Vec::with_capacity(slots.len());
    for (idx, slot) in slots.into_iter().enumerate() {
        if let Some(slot) = slot {
            out.push(slot);
        } else {
            signature[idx].default.as_ref()?;
            out.push(ArgSlot::NeedsDefault(idx));
        }
    }
    Some(out)
}

/// A resolved overload plus its bound argument values.
type ResolvedRoutine = (Arc<SQLUserFunction>, Vec<Value>);

/// Resolve `name(args)` to a single overload and its bound argument
/// values (declared-type casts applied, defaults evaluated).
/// `Ok(None)` = no routine with this name at all.
pub(super) fn resolve_routine(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
    declared_argument_types: Option<&[Option<ColumnType>]>,
    kind: &str,
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
    let function = engine
        .resolve_static_sql_routine(name, None, &argument_names, argument_types, call_kind)?
        .ok_or_else(|| routine_resolution_error(kind, name, args, "does not exist"))?;
    let slots = try_match_arguments(&function.def, args).ok_or_else(|| {
        SQLError::Internal("resolved routine no longer matches its runtime arguments".into())
    })?;
    let bound = materialize_arguments(engine, &function.def, slots)?;
    Ok(Some((function, bound)))
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
    let function = engine
        .resolve_static_sql_routine(
            &binding.name,
            Some(binding),
            &argument_names,
            &argument_types,
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
    let slots = try_match_arguments(&function.def, args).ok_or_else(|| {
        routine_resolution_error("function", &binding.name, args, "does not exist")
    })?;
    let bound = materialize_arguments(engine, &function.def, slots)?;
    Ok(Some((function, bound)))
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
    slots: Vec<ArgSlot>,
) -> Result<Vec<Value>, SQLError> {
    let signature = def.signature_params();
    let mut bound = Vec::with_capacity(slots.len());
    for (idx, slot) in slots.into_iter().enumerate() {
        let value = match slot {
            ArgSlot::Filled(value) => value,
            ArgSlot::NeedsDefault(param_idx) => {
                let default = signature[param_idx].default.as_ref().ok_or_else(|| {
                    SQLError::Internal("argument default vanished during resolution".into())
                })?;
                eval_lowered_expression(engine, default, None, &[])?
            }
        };
        bound.push(coerce_routine_value(
            engine,
            &value,
            &signature[idx].type_name,
        )?);
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
