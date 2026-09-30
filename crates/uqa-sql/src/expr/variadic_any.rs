//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-ins declared `VARIADIC "any"`: an explicit `VARIADIC array` argument supplies the variadic values element by element, flattening every dimension, as `PostgreSQL`'s `extract_variadic_args` does.

use uqa_core::Value;

use super::{Result, SQLError};

/// A call's evaluated arguments after an explicit `VARIADIC` array has been expanded.
pub enum VariadicAnyArguments {
    Arguments(Vec<(Option<String>, Value)>),
    /// A NULL `VARIADIC` array makes the function return NULL without reading any value.
    NullResult,
}

fn local_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .map_or_else(|| lower.clone(), str::to_owned)
}

/// The number of fixed parameters before the `VARIADIC "any"` parameter, or `None` for any other routine.
fn fixed_parameters(name: &str) -> Option<usize> {
    Some(match local_name(name).as_str() {
        "concat" | "json_build_object" | "json_build_array" | "jsonb_build_object"
        | "jsonb_build_array" | "num_nulls" | "num_nonnulls" => 0,
        "concat_ws" | "format" => 1,
        _ => return None,
    })
}

/// Whether a built-in declares a `VARIADIC "any"` parameter.
#[must_use]
pub fn is_variadic_any(name: &str) -> bool {
    fixed_parameters(name).is_some()
}

pub(crate) fn not_an_array() -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: "VARIADIC argument must be an array".into(),
    }
}

fn flatten(values: &[Value], output: &mut Vec<(Option<String>, Value)>) {
    for value in values {
        match value {
            Value::List(values) => flatten(values, output),
            value => output.push((None, value.clone())),
        }
    }
}

/// Expand the final explicit `VARIADIC` argument of a `VARIADIC "any"` built-in. Other calls keep their arguments.
pub fn expand_variadic_any(
    name: &str,
    mut arguments: Vec<(Option<String>, Value)>,
) -> Result<VariadicAnyArguments> {
    let Some(fixed) = fixed_parameters(name) else {
        return Ok(VariadicAnyArguments::Arguments(arguments));
    };
    if arguments.len() != fixed + 1 {
        return Ok(VariadicAnyArguments::Arguments(arguments));
    }
    let Some((_, variadic)) = arguments.pop() else {
        return Ok(VariadicAnyArguments::Arguments(arguments));
    };
    match variadic {
        // `format` treats a NULL array as zero values; the other functions return NULL.
        Value::Null if local_name(name) == "format" => {
            Ok(VariadicAnyArguments::Arguments(arguments))
        }
        Value::Null => Ok(VariadicAnyArguments::NullResult),
        Value::Array(array) => {
            flatten(array.elements(), &mut arguments);
            Ok(VariadicAnyArguments::Arguments(arguments))
        }
        Value::List(values) => {
            flatten(&values, &mut arguments);
            Ok(VariadicAnyArguments::Arguments(arguments))
        }
        _ => Err(not_an_array()),
    }
}
