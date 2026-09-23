//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named argument selection retains reordered values under the caller's allowance.

use super::super::{array_transform, json_strip};
use crate::error::Result;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) fn builtin_named_args(
    function: &str,
    call_args: &[(Option<String>, Value)],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>> {
    control.check()?;
    if matches!(function, "array_sort" | "array_reverse") {
        return array_transform::reorder_named_values_with_control(function, call_args, control);
    }
    if matches!(function, "json_strip_nulls" | "jsonb_strip_nulls") {
        return json_strip::reorder_named_values_with_control(function, call_args, control);
    }
    let names: &[&str] = match function {
        "regexp_count" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "flags"],
            _ => return Ok(None),
        },
        "regexp_like" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "flags"],
            _ => return Ok(None),
        },
        "regexp_substr" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "N"],
            5 => &["string", "pattern", "start", "N", "flags"],
            6 => &["string", "pattern", "start", "N", "flags", "subexpr"],
            _ => return Ok(None),
        },
        "regexp_instr" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "N"],
            5 => &["string", "pattern", "start", "N", "endoption"],
            6 => &["string", "pattern", "start", "N", "endoption", "flags"],
            7 => &[
                "string",
                "pattern",
                "start",
                "N",
                "endoption",
                "flags",
                "subexpr",
            ],
            _ => return Ok(None),
        },
        "regexp_replace" => match call_args.len() {
            3 => &["string", "pattern", "replacement"],
            4 if call_args
                .iter()
                .any(|(name, _)| name.as_deref() == Some("flags")) =>
            {
                &["string", "pattern", "replacement", "flags"]
            }
            4 => &["string", "pattern", "replacement", "start"],
            5 => &["string", "pattern", "replacement", "start", "N"],
            6 => &["string", "pattern", "replacement", "start", "N", "flags"],
            _ => return Ok(None),
        },
        "make_interval" => return make_interval_named_args(call_args, control),
        _ => return Ok(None),
    };
    reorder_named_args(call_args, names, control)
}

fn reorder_named_args(
    call_args: &[(Option<String>, Value)],
    parameter_names: &[&str],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>> {
    if call_args.len() != parameter_names.len() {
        return Ok(None);
    }
    let Some(slots) = select_slots(call_args, parameter_names, control)? else {
        return Ok(None);
    };
    copy_slots(&slots, None, control)
}

fn select_slots<'a>(
    call_args: &'a [(Option<String>, Value)],
    parameter_names: &[&str],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Option<&'a Value>>>>> {
    let mut slots = ProductionVec::new(*control);
    slots.reserve(parameter_names.len())?;
    for _ in parameter_names {
        slots.push_copy(None)?;
    }
    let mut slots = slots.finish()?;
    let mut positional_index = 0;
    let mut saw_named = false;
    for (name, value) in call_args {
        control.check()?;
        let slot = if let Some(name) = name {
            saw_named = true;
            let Some(slot) = parameter_names
                .iter()
                .position(|candidate| candidate == name)
            else {
                return Ok(None);
            };
            slot
        } else {
            if saw_named {
                return Ok(None);
            }
            let slot = positional_index;
            positional_index += 1;
            slot
        };
        let Some(selected) = slots.as_mut_slice().get_mut(slot) else {
            return Ok(None);
        };
        if selected.is_some() {
            return Ok(None);
        }
        *selected = Some(value);
    }
    Ok(Some(slots))
}

fn copy_slots(
    slots: &[Option<&Value>],
    default: Option<&Value>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(slots.len())?;
    for selected in slots {
        let Some(value) = selected.or(default) else {
            return Ok(None);
        };
        output.push_produced(control.copy_value(value)?)?;
    }
    Ok(Some(output.finish()?))
}

/// Map the existing interval names and omitted zero defaults to declaration order.
fn make_interval_named_args(
    call_args: &[(Option<String>, Value)],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>> {
    const NAMES: [&str; 7] = ["years", "months", "weeks", "days", "hours", "mins", "secs"];
    let Some(slots) = select_slots(call_args, &NAMES, control)? else {
        return Ok(None);
    };
    copy_slots(&slots, Some(&Value::Int(0)), control)
}

#[cfg(test)]
mod tests;
