//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` 18 signatures for array transformations.

use super::{validate_named_argument_order, Result, Value};

/// Physical marker preserving a one-dimensional `json[]` input after the polymorphic `array_sort(anyarray, ...)` call has been bound.
pub const ARRAY_SORT_JSON_FUNCTION: &str = "__array_sort_json";

/// Map call-order arguments onto the declared `array_sort` and `array_reverse` slots. `None` means the arity or a named argument does not select a catalogued overload.
pub fn argument_positions(
    name: &str,
    argument_names: &[Option<&str>],
) -> Result<Option<Vec<usize>>> {
    validate_named_argument_order(argument_names.iter().copied())?;
    let lower = name.to_ascii_lowercase();
    let function = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    let parameter_names: &[Option<&str>] = match (function, argument_names.len()) {
        ("array_reverse", 1) | ("array_sort", 1) => &[None],
        ("array_sort", 2) => &[Some("array"), Some("descending")],
        ("array_sort", 3) => &[Some("array"), Some("descending"), Some("nulls_first")],
        _ => return Ok(None),
    };
    let mut occupied = vec![false; parameter_names.len()];
    let mut positions = Vec::with_capacity(argument_names.len());
    let mut positional = 0;
    for argument_name in argument_names {
        let position = if let Some(argument_name) = argument_name {
            parameter_names
                .iter()
                .position(|candidate| *candidate == Some(*argument_name))
        } else {
            let position = positional;
            positional += 1;
            Some(position)
        };
        let Some(position) = position.filter(|position| *position < occupied.len()) else {
            return Ok(None);
        };
        if occupied[position] {
            return Ok(None);
        }
        occupied[position] = true;
        positions.push(position);
    }
    Ok(occupied.into_iter().all(|slot| slot).then_some(positions))
}

pub(super) fn reorder_named_values(
    function: &str,
    call_args: &[(Option<String>, Value)],
) -> Option<Vec<Value>> {
    let argument_names = call_args
        .iter()
        .map(|(name, _)| name.as_deref())
        .collect::<Vec<_>>();
    let positions = argument_positions(function, &argument_names)
        .ok()
        .flatten()?;
    let mut values = vec![None; call_args.len()];
    for ((_, value), position) in call_args.iter().zip(positions) {
        values[position] = Some(value.clone());
    }
    values.into_iter().collect()
}
