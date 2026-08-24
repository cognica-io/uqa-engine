//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column binding for fixed-signature `PostgreSQL` built-ins.

use crate::sql::{Engine, SQLError};
use uqa_sql::ast::{ColumnDef, Expr, FunctionBinding, GeneratedFunctionDependency};

use super::super::{
    generation_expression_column_type, named_argument, non_immutable_function,
    validate_bound_function, validate_unknown_literal_cast, GenerationType,
};

pub(in super::super) struct FixedBuiltinCall<'a> {
    pub(in super::super) engine: &'a Engine,
    pub(in super::super) columns: &'a [ColumnDef],
    pub(in super::super) name: &'a str,
    pub(in super::super) args: &'a [Expr],
    pub(in super::super) argument_names: &'a [Option<String>],
    pub(in super::super) argument_types: &'a [GenerationType],
}

pub(in super::super) fn bind_call(
    call: FixedBuiltinCall<'_>,
    binding: &mut Option<FunctionBinding>,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<bool, SQLError> {
    let declared_argument_types = call
        .args
        .iter()
        .zip(call.argument_types)
        .map(|(argument, inferred)| {
            let (_, value) = named_argument(argument)?;
            Ok(generation_expression_column_type(
                call.columns,
                value,
                inferred,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let Some(resolved) = uqa_execution::resolve_fixed_builtin_call(
        call.name,
        binding.as_ref(),
        call.argument_names,
        &declared_argument_types,
        Some(call.engine),
    )?
    else {
        return Ok(false);
    };
    let selected = resolved.selected;
    if !selected.binding.builtin {
        let selected = validate_bound_function(
            call.engine,
            &selected.binding,
            call.argument_names,
            call.argument_types,
        )?;
        dependencies.push(selected.clone());
        *binding = Some(selected);
        return Ok(true);
    }
    if resolved.builtin_volatile {
        return Err(non_immutable_function(call.name));
    }
    let positions = resolved.builtin_argument_positions.ok_or_else(|| {
        SQLError::Internal(format!(
            "resolved generated-column built-in `{}` lost its argument mapping",
            selected.binding.name
        ))
    })?;
    for (actual, position) in call.argument_types.iter().zip(positions) {
        validate_unknown_literal_cast(actual, &selected.binding.argument_types[position])?;
    }
    *binding = Some(selected.binding);
    Ok(true)
}
