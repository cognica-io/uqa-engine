//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column binding for `json_strip_nulls` and `jsonb_strip_nulls`.

use crate::sql::{Engine, SQLError};
use uqa_sql::ast::{ColumnDef, Expr, FunctionBinding, GeneratedFunctionDependency};

use super::super::{
    generation_type_name, named_argument, validate_unknown_literal_cast, GenerationType,
};
use super::string_binary::{generation_expression_column_type, validate_bound_function};

pub(in super::super) struct JsonStripCall<'a> {
    pub(in super::super) engine: &'a Engine,
    pub(in super::super) columns: &'a [ColumnDef],
    pub(in super::super) name: &'a str,
    pub(in super::super) args: &'a [Expr],
    pub(in super::super) argument_names: &'a [Option<String>],
    pub(in super::super) argument_types: &'a [GenerationType],
}

pub(in super::super) fn bind_call(
    call: JsonStripCall<'_>,
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
    let selected = uqa_execution::resolve_json_strip_overload(
        call.name,
        binding.as_ref(),
        call.argument_names,
        &declared_argument_types,
        Some(call.engine),
    )?;
    if selected.binding.builtin {
        let names = call
            .argument_names
            .iter()
            .map(Option::as_deref)
            .collect::<Vec<_>>();
        let positions = uqa_sql::expr::json_strip_nulls_argument_positions(call.name, &names)?
            .ok_or_else(|| {
                undefined_function(call.name, call.argument_names, call.argument_types)
            })?;
        for (actual, position) in call.argument_types.iter().zip(positions) {
            validate_unknown_literal_cast(actual, &selected.binding.argument_types[position])?;
        }
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

pub(super) fn require_signature(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> Result<GenerationType, SQLError> {
    let names = argument_names
        .iter()
        .map(Option::as_deref)
        .collect::<Vec<_>>();
    let Some(positions) = uqa_sql::expr::json_strip_nulls_argument_positions(name, &names)? else {
        return Err(undefined_function(name, argument_names, args));
    };
    let target = if local_name(name) == "jsonb_strip_nulls" {
        GenerationType::JsonB
    } else {
        GenerationType::Json
    };
    for (actual, position) in args.iter().zip(positions) {
        let accepted = match position {
            0 => {
                matches!(
                    actual,
                    GenerationType::Null | GenerationType::UnknownLiteral(_)
                ) || actual == &target
            }
            1 => matches!(
                actual,
                GenerationType::Null | GenerationType::UnknownLiteral(_) | GenerationType::Boolean
            ),
            _ => false,
        };
        if !accepted {
            return Err(undefined_function(name, argument_names, args));
        }
    }
    Ok(target)
}

fn is_function(name: &str) -> bool {
    matches!(
        local_name(name).as_str(),
        "json_strip_nulls" | "jsonb_strip_nulls"
    )
}

fn local_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .unwrap_or(&lower)
        .to_string()
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> SQLError {
    let signature = args
        .iter()
        .zip(argument_names)
        .map(|(argument, argument_name)| {
            let argument = generation_type_name(argument);
            argument_name
                .as_ref()
                .map_or(argument.clone(), |name| format!("{name} => {argument}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}
