//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only argument selection is shared by ordinary and mutable-owner call construction.

use super::{
    argument_names_with_control, is_json_array_type, local_name, named_argument_value, ColumnType,
    FunctionBinding, FunctionDispatch, InferType, Produced, ProductionControl, SQLError,
    ScalarExpr, Value,
};

pub(super) struct Prepared {
    pub(super) positions: Produced<Vec<usize>>,
    pub(super) cast_names: [Option<Produced<String>>; 3],
    pub(super) selected: Option<Produced<FunctionBinding>>,
}

pub(super) fn prepare(
    name: &str,
    arguments: &[ScalarExpr],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Prepared>, SQLError> {
    for argument in arguments {
        if crate::scalar_call_argument(argument).is_err() {
            return Ok(None);
        }
    }
    let positions = {
        let names = argument_names_with_control(arguments, control)?;
        match crate::expr::array_transform_argument_positions_with_control(name, &names, control) {
            Ok(Some(positions)) => positions,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Ok(None) | Err(_) => return Ok(None),
        }
    };
    let mut cast_names: [Option<Produced<String>>; 3] = [None, None, None];
    let mut selected = None;
    for (argument, position) in arguments.iter().zip(positions.iter().copied()) {
        let argument = named_argument_value(argument);
        let ty = if position > 0
            && matches!(
                argument,
                ScalarExpr::Param(_) | ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
            None
        } else {
            match crate::type_resolution::call::infer_with_control(argument, infer, control) {
                Ok(ty) => ty,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => None,
            }
        };
        if position > 0 && ty.is_none() {
            cast_names[position] = Some(ColumnType::Boolean.catalog_name_with_control(control)?);
        } else if position == 0
            && local_name(name).eq_ignore_ascii_case("array_sort")
            && ty.as_deref().is_some_and(is_json_array_type)
        {
            selected = Some(FunctionBinding::dispatched_with_control(
                FunctionDispatch::ArraySortJson,
                control,
            )?);
        }
    }
    Ok(Some(Prepared {
        positions,
        cast_names,
        selected,
    }))
}
