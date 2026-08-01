//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine overload resolution, argument binding, and coercion.

use super::{
    canonical_routine_type_name, cast_value, eval_lowered_expression, value_type_name, Arc,
    CreateFunction, Engine, SQLError, SQLUserFunction, Value,
};

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

fn argument_type_cost(value: &Value, declared_type: &str) -> Option<u32> {
    if matches!(value, Value::Null) {
        // NULL has no concrete input type, so it cannot prefer one overload
        // over another but remains coercible to every declared type.
        return Some(1);
    }
    let actual = canonical_routine_type_name(value_type_name(value));
    let declared = canonical_routine_type_name(declared_type);
    if actual == declared {
        return Some(0);
    }
    let actual_is_numeric = matches!(
        actual.as_str(),
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric"
    );
    let declared_is_numeric = matches!(
        declared.as_str(),
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric"
    );
    if actual_is_numeric && declared_is_numeric {
        return best_effort_cast(value, declared_type).ok().map(|_| 1);
    }
    best_effort_cast(value, declared_type).ok().map(|_| 2)
}

fn overload_match_cost(def: &CreateFunction, slots: &[ArgSlot]) -> Option<u32> {
    let signature = def.signature_params();
    let mut cost = 0_u32;
    for (parameter, slot) in signature.iter().zip(slots) {
        if let ArgSlot::Filled(value) = slot {
            cost = cost.checked_add(argument_type_cost(value, &parameter.type_name)?)?;
        }
    }
    Some(cost)
}

/// Match a call's argument list against a routine signature without
/// evaluating defaults. `None` = not a candidate.
fn try_match_arguments(
    def: &CreateFunction,
    args: &[(Option<String>, Value)],
) -> Option<Vec<ArgSlot>> {
    let signature = def.signature_params();
    if args.len() > signature.len() {
        return None;
    }
    let mut slots: Vec<Option<ArgSlot>> = (0..signature.len()).map(|_| None).collect();
    let mut position = 0usize;
    let mut seen_named = false;
    for (arg_name, value) in args {
        match arg_name {
            None => {
                if seen_named || position >= signature.len() {
                    return None;
                }
                slots[position] = Some(ArgSlot::Filled(value.clone()));
                position += 1;
            }
            Some(arg_name) => {
                seen_named = true;
                let idx = signature
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(arg_name))?;
                if slots[idx].is_some() {
                    return None;
                }
                slots[idx] = Some(ArgSlot::Filled(value.clone()));
            }
        }
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
    kind: &str,
) -> Result<Option<ResolvedRoutine>, SQLError> {
    let Some(overloads) = engine.lookup_sql_functions(name) else {
        return Ok(None);
    };
    let requested_is_procedure = kind == "procedure";
    let mut candidates: Vec<(Arc<SQLUserFunction>, Vec<ArgSlot>, u32)> = Vec::new();
    for function in overloads {
        if let Some(slots) = try_match_arguments(&function.def, args) {
            if let Some(cost) = overload_match_cost(&function.def, &slots) {
                candidates.push((function, slots, cost));
            }
        }
    }
    if candidates.is_empty() {
        return Err(routine_resolution_error(kind, name, args, "does not exist"));
    }
    let has_requested_kind = candidates
        .iter()
        .any(|(function, _, _)| function.def.is_procedure == requested_is_procedure);
    if has_requested_kind {
        candidates.retain(|(function, _, _)| function.def.is_procedure == requested_is_procedure);
    }
    let best_cost = candidates
        .iter()
        .map(|(_, _, cost)| *cost)
        .min()
        .ok_or_else(|| SQLError::Internal("routine candidate set lost its score".into()))?;
    candidates.retain(|(_, _, cost)| *cost == best_cost);
    if candidates.len() != 1 {
        return Err(routine_resolution_error(kind, name, args, "is not unique"));
    }
    let (function, slots, _) = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("winning routine candidate disappeared".into()))?;
    let bound = materialize_arguments(engine, &function.def, slots)?;
    Ok(Some((function, bound)))
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
        bound.push(best_effort_cast(&value, &signature[idx].type_name)?);
    }
    Ok(bound)
}

/// Cast through the SQL value layer; unknown target types keep the
/// value unchanged (`%TYPE`, `record`, domain names, ...).
pub(super) fn best_effort_cast(value: &Value, type_name: &str) -> Result<Value, SQLError> {
    match cast_value(value, type_name) {
        Ok(value) => Ok(value),
        Err(SQLError::Unsupported(_)) => Ok(value.clone()),
        Err(e) => Err(e),
    }
}
