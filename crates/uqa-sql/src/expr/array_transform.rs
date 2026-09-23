//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` 18 signatures for array transformations.

use super::{validate_named_argument_order_with_control, Result, Value};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// Map call-order arguments onto the declared `array_sort` and `array_reverse` slots. `None` means the arity or a named argument does not select a catalogued overload.
pub fn argument_positions(
    name: &str,
    argument_names: &[Option<&str>],
) -> Result<Option<Vec<usize>>> {
    argument_positions_with_control(name, argument_names, &ProductionControl::uncontrolled()).map(
        |positions| {
            positions.map(|positions| {
                positions
                    .into_uncontrolled()
                    .expect("ordinary argument positions")
            })
        },
    )
}

pub fn argument_positions_with_control(
    name: &str,
    argument_names: &[Option<&str>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<usize>>>> {
    control.check()?;
    validate_named_argument_order_with_control(argument_names.iter().copied(), control)?;
    let function = name
        .get(..11)
        .filter(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
        .map_or(name, |_| &name[11..]);
    let parameter_names: &[Option<&str>] = if argument_names.len() == 1
        && (function.eq_ignore_ascii_case("array_reverse")
            || function.eq_ignore_ascii_case("array_sort"))
    {
        &[None]
    } else if function.eq_ignore_ascii_case("array_sort") {
        match argument_names.len() {
            2 => &[Some("array"), Some("descending")],
            3 => &[Some("array"), Some("descending"), Some("nulls_first")],
            _ => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    let mut occupied = [false; 3];
    let mut positions = ProductionVec::new(*control);
    positions.reserve(argument_names.len())?;
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
        let Some(position) = position.filter(|position| *position < parameter_names.len()) else {
            return Ok(None);
        };
        if occupied[position] {
            return Ok(None);
        }
        occupied[position] = true;
        positions.push_copy(position)?;
    }
    Ok(occupied[..parameter_names.len()]
        .iter()
        .all(|slot| *slot)
        .then(|| positions.finish())
        .transpose()?)
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
