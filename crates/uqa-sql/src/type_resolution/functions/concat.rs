//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the array overload of `||` before runtime values erase scalar/array NULL identity.

use crate::{ast::FunctionDispatch, ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

use super::super::{array_element_type, common::common_type_with_control};

pub(in crate::type_resolution) fn array_concat_types(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<(FunctionDispatch, Produced<ColumnType>)>, SQLError> {
    control.check()?;
    let left_element = left.and_then(element);
    let right_element = right.and_then(element);
    let dispatch = match (left_element.is_some(), right_element.is_some()) {
        (true, true) => FunctionDispatch::ArrayConcat,
        (true, false) if right.is_none() => FunctionDispatch::ArrayConcat,
        (false, true) if left.is_none() => FunctionDispatch::ArrayConcat,
        (true, false) => FunctionDispatch::ArrayAppend,
        (false, true) => FunctionDispatch::ArrayPrepend,
        (false, false) => return Ok(None),
    };
    let selected = match (left_element.or(left), right_element.or(right)) {
        (Some(left_element), Some(right_element)) => {
            common_type_with_control(left_element, right_element, control).map_err(|error| {
                if error.sqlstate() == Some("42804") {
                    super::super::undefined_binary_operator(left, "||", right)
                } else {
                    error
                }
            })?
        }
        (Some(element), None) | (None, Some(element)) => element.clone_with_control(control)?,
        (None, None) => unreachable!("an array operand supplies its element type"),
    };
    let selected = selected.without_type_modifiers_with_control(control)?;
    Ok(Some((
        dispatch,
        ColumnType::array_with_control(selected, control)?,
    )))
}

fn element(ty: &ColumnType) -> Option<&ColumnType> {
    let mut element = array_element_type(ty)?;
    // Declared rank is not part of an array type's identity. Domains over scalar elements remain intact.
    while let ColumnType::Array(inner) = element {
        element = inner;
    }
    Some(element)
}

pub(in crate::type_resolution) fn array_concat_argument(
    dispatch: FunctionDispatch,
    position: usize,
) -> bool {
    match dispatch {
        FunctionDispatch::ArrayConcat => true,
        FunctionDispatch::ArrayAppend => position == 0,
        FunctionDispatch::ArrayPrepend => position == 1,
        _ => unreachable!("selected array concatenation operation"),
    }
}

#[cfg(test)]
mod tests;
