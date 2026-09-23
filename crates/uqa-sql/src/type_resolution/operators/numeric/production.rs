//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric operator selection retains its argument container and uses the shared borrowed matcher.

use super::{NumericOperator, NumericOperatorTypes, PREFIX_SIGNATURES};
use crate::{
    type_resolution::{
        common::base_type,
        operators::{named_binary_operator_catalog_entry, resolution},
        overload_resolution::select_local_builtin_with_control,
        BuiltinFunctionOverload,
    },
    ColumnType, SQLError,
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl, ProductionVec};

/// Select from the existing numeric operator catalog with admitted argument, candidate and result ownership.
pub fn numeric_operator_types_with_control(
    operator: NumericOperator,
    arguments: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<NumericOperatorTypes>, SQLError> {
    control.check()?;
    if arguments.len() != operator.arity() {
        return Err(SQLError::Internal(format!(
            "operator {} expects {} operands, got {}",
            operator.symbol(),
            operator.arity(),
            arguments.len()
        )));
    }
    if let [left, right] = arguments {
        let types = resolution::named_binary_operator_types_with_control(
            operator.symbol(),
            left.as_ref(),
            right.as_ref(),
            control,
        )?;
        let entry = named_binary_operator_catalog_entry(operator.symbol(), [&types[0], &types[1]])?;
        let values = argument_buffer(2, control)?;
        // The complete two-slot buffer is already admitted. Moving the selected type fields into it performs no allocation or fallible operation before the final composite owner is constructed.
        let (mut values, values_memory) = values.into_parts();
        let ([left, right, result], type_memory) = types.into_parts();
        values.push(left);
        values.push(right);
        return control
            .finish(
                NumericOperatorTypes {
                    arguments: values,
                    result,
                    oid: entry.oid,
                    function_oid: entry.function_oid,
                },
                control.combine(values_memory, type_memory),
            )
            .map_err(Into::into);
    }
    prefix_operator_types(operator, arguments[0].as_ref(), control)
}

fn prefix_operator_types(
    operator: NumericOperator,
    argument: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<NumericOperatorTypes>, SQLError> {
    let argument = argument
        .map(|ty| base_type(ty).without_type_modifiers_with_control(control))
        .transpose()?;
    let (argument, memory) = argument.map_or_else(
        || (None, control.empty_reservation()),
        |argument| {
            let (argument, memory) = argument.into_parts();
            (Some(argument), memory)
        },
    );
    let argument = control.finish([argument], memory)?;
    let mut candidates = ProductionVec::new(*control);
    for &(name, ty, ..) in PREFIX_SIGNATURES {
        control.check()?;
        if name == operator.symbol() {
            let ty = resolution::catalog_type_with_control(ty, control)?
                .expect("static prefix operator type");
            candidates.push_produced(prefix_candidate(operator.symbol(), ty, control)?)?;
        }
    }
    let candidates = candidates.finish()?;
    let selected = select_local_builtin_with_control(
        operator.symbol(),
        None,
        &[None],
        &*argument,
        &candidates,
        control,
    )
    .map_err(|error| {
        if matches!(error.sqlstate(), Some("53200" | "57014")) {
            return error;
        }
        let ambiguous = error.sqlstate() == Some("42725");
        SQLError::Routine {
            sqlstate: if ambiguous { "42725" } else { "42883" }.into(),
            message: format!(
                "operator {}: {} {}",
                if ambiguous {
                    "is not unique"
                } else {
                    "does not exist"
                },
                operator.symbol(),
                argument[0]
                    .as_ref()
                    .map_or_else(|| "unknown".into(), ColumnType::sql_name)
            ),
        }
    })?;
    let result = &selected.builtin.return_type;
    let mut identity = None;
    for &(name, ty, oid, function_oid) in PREFIX_SIGNATURES {
        control.check()?;
        if name == operator.symbol()
            && resolution::catalog_type_with_control(ty, control)?.as_deref() == Some(result)
        {
            identity = Some((oid, function_oid));
            break;
        }
    }
    let (oid, function_oid) = identity.expect("selected prefix operator identity");
    let mut values = ProductionVec::new(*control);
    values.reserve(1)?;
    values.push_produced(result.clone_with_control(control)?)?;
    let values = values.finish()?;
    let result = result.clone_with_control(control)?;
    let (arguments, arguments_memory) = values.into_parts();
    let (result, result_memory) = result.into_parts();
    control
        .finish(
            NumericOperatorTypes {
                arguments,
                result,
                oid,
                function_oid,
            },
            control.combine(arguments_memory, result_memory),
        )
        .map_err(Into::into)
}

fn prefix_candidate(
    name: &str,
    ty: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<BuiltinFunctionOverload>, SQLError> {
    let name = control.copy_text(name)?;
    let result = ty.clone_with_control(control)?;
    let mut names = ProductionVec::new(*control);
    names.push_produced(control.finish(None, control.empty_reservation())?)?;
    let names = names.finish()?;
    let mut types = ProductionVec::new(*control);
    types.push_produced(ty)?;
    let types = types.finish()?;
    let (name, name_memory) = name.into_parts();
    let (argument_names, labels_memory) = names.into_parts();
    let (argument_types, types_memory) = types.into_parts();
    let (return_type, result_memory) = result.into_parts();
    control
        .finish(
            BuiltinFunctionOverload {
                name,
                argument_names,
                argument_types,
                default_arguments: 0,
                return_type,
            },
            control.combine(
                control.combine(name_memory, labels_memory),
                control.combine(types_memory, result_memory),
            ),
        )
        .map_err(Into::into)
}

fn argument_buffer(
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ColumnType>>, SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            let mut values = BudgetedVec::new(budget);
            values.reserve(count)?;
            let (values, memory) = values.into_parts();
            control.finish(values, Some(memory)).map_err(Into::into)
        }
        None => control
            .finish(Vec::with_capacity(count), None)
            .map_err(Into::into),
    }
}

#[cfg(test)]
mod tests;
