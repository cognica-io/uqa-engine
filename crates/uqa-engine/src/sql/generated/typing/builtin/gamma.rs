//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column binding for `gamma(float8)` and `lgamma(float8)`.

use crate::sql::{Engine, SQLError};
use uqa_sql::ast::{ColumnDef, Expr, FunctionBinding, GeneratedFunctionDependency};

use super::super::{named_argument, validate_unknown_literal_cast, GenerationType};
use super::string_binary::{generation_expression_column_type, validate_bound_function};

pub(in super::super) struct GammaCall<'a> {
    pub(in super::super) engine: &'a Engine,
    pub(in super::super) columns: &'a [ColumnDef],
    pub(in super::super) name: &'a str,
    pub(in super::super) args: &'a [Expr],
    pub(in super::super) argument_names: &'a [Option<String>],
    pub(in super::super) argument_types: &'a [GenerationType],
}

pub(in super::super) fn bind_call(
    call: GammaCall<'_>,
    binding: &mut Option<FunctionBinding>,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<bool, SQLError> {
    if !is_function(call.name) {
        return Ok(false);
    }
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
    let selected = uqa_execution::resolve_gamma_overload(
        call.name,
        binding.as_ref(),
        call.argument_names,
        &declared_argument_types,
        Some(call.engine),
    )?;
    for (actual, declared) in call
        .argument_types
        .iter()
        .zip(&selected.binding.argument_types)
    {
        validate_unknown_literal_cast(actual, declared)?;
    }
    if selected.binding.builtin {
        *binding = Some(selected.binding);
    } else {
        let selected = validate_bound_function(
            call.engine,
            &selected.binding,
            call.argument_names,
            call.argument_types,
        )?;
        dependencies.push(selected.clone());
        *binding = Some(selected);
    }
    Ok(true)
}

fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "gamma" | "lgamma"
    )
}
