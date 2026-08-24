//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One registry and binding path for implemented fixed-signature `PostgreSQL` built-ins.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::expr::{
    RANDOM_INT4_FUNCTION, RANDOM_INT8_FUNCTION, RANDOM_NUMERIC_FUNCTION, TO_BIN_INT4_FUNCTION,
    TO_BIN_INT8_FUNCTION, TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION, TO_OCT_INT4_FUNCTION,
    TO_OCT_INT8_FUNCTION, UNDEFINED_FUNCTION_MARKER,
};
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

use super::common::base_type;
use super::functions::{named_argument, named_argument_value};
use super::{
    builtin_binding_matches, canonical_column_type_name, canonical_routine_type_name,
    match_builtin_function_overload, resolve_local_builtin_overload, scalar_type_inner,
    BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload,
};

pub(super) fn is_function(name: &str) -> bool {
    overloads(name).is_some()
}

/// Fixed-signature call metadata needed by generated-column binding without exposing the built-in registry itself.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFixedBuiltinCall {
    pub selected: ResolvedFunctionOverload,
    pub builtin_argument_positions: Option<Vec<usize>>,
    pub builtin_volatile: bool,
}

/// Resolve an implemented fixed-signature built-in together with visible SQL routine overloads. `None` means `name` is outside the fixed registry.
#[doc(hidden)]
pub fn resolve_fixed_builtin_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFixedBuiltinCall>, SQLError> {
    let Some(builtins) = overloads(name) else {
        return Ok(None);
    };
    let selected = resolve_overload(name, binding, argument_names, argument_types, resolver)?;
    let (builtin_argument_positions, builtin_volatile) = if selected.binding.builtin {
        let matched = builtins
            .iter()
            .find(|overload| builtin_binding_matches(overload, &selected.binding))
            .cloned()
            .and_then(|overload| {
                match_builtin_function_overload(overload, argument_names, argument_types)
            })
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved fixed built-in `{}` lost its catalog signature",
                    selected.binding.name
                ))
            })?;
        (
            Some(matched.argument_positions),
            builtin_binding_is_volatile(&selected.binding),
        )
    } else {
        (None, false)
    };
    Ok(Some(ResolvedFixedBuiltinCall {
        selected,
        builtin_argument_positions,
        builtin_volatile,
    }))
}

/// Return the catalog result type encoded by a stable fixed built-in binding.
#[doc(hidden)]
#[must_use]
pub fn fixed_builtin_return_type(binding: &FunctionBinding) -> Option<ColumnType> {
    binding.builtin.then_some(())?;
    overloads(&binding.name)?
        .into_iter()
        .find(|overload| builtin_binding_matches(overload, binding))
        .map(|overload| overload.return_type)
}

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let argument_names = argument_names(args);
    let argument_types = effective_argument_types(args, argument_types);
    resolve_overload(name, binding, &argument_names, &argument_types, resolver)
        .map(|overload| Some(overload.return_type))
}

pub(super) fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedFunctionOverload, SQLError> {
    uqa_sql::expr::validate_named_argument_order(argument_names.iter().map(Option::as_deref))?;
    let builtins = overloads(name).ok_or_else(|| {
        super::function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        )
    })?;
    if let Some(resolver) = resolver {
        if let Some(selected) = resolver.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            &builtins,
        )? {
            return Ok(selected);
        }
    }
    resolve_local_builtin_overload(name, binding, argument_names, argument_types, &builtins)
}

pub(super) fn selected_argument_targets(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Option<Vec<Option<ColumnType>>> {
    let argument_names = vec![None; argument_types.len()];
    let selected = resolve_overload(name, None, &argument_names, argument_types, None).ok()?;
    let declared = selected
        .binding
        .argument_types
        .iter()
        .map(|ty| ColumnType::from_sql_name(ty).ok())
        .take(argument_types.len())
        .collect::<Vec<_>>();
    (declared.len() == argument_types.len()).then_some(declared)
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    let Some(builtins) = overloads(&name) else {
        return name;
    };
    let Ok(argument_types) = args
        .iter()
        .map(|argument| scalar_type_inner(named_argument_value(argument), schema, params, resolver))
        .collect::<Result<Vec<_>, _>>()
    else {
        return name;
    };
    let names = argument_names(args);
    let argument_types = effective_argument_types(args, &argument_types);
    let selected = resolve_overload(&name, binding.as_ref(), &names, &argument_types, resolver);
    let selected = match selected {
        Ok(selected) => selected,
        Err(error) if error.sqlstate() == Some("42883") => {
            return unresolved_call_name(&name, &names, &argument_types);
        }
        Err(_) => return name,
    };
    if !selected.binding.builtin {
        *binding = Some(selected.binding);
        return name;
    }
    let Some(matched) = builtins
        .iter()
        .find(|overload| builtin_binding_matches(overload, &selected.binding))
        .cloned()
        .and_then(|overload| match_builtin_function_overload(overload, &names, &argument_types))
    else {
        return name;
    };
    let overload = matched.overload;
    let mut reordered = vec![None; overload.argument_types.len()];
    for (argument, position) in std::mem::take(args)
        .into_iter()
        .zip(matched.argument_positions)
    {
        reordered[position] = Some(named_argument_value_owned(argument));
    }
    for (position, argument) in reordered.iter_mut().enumerate() {
        if argument.is_none() {
            *argument = default_argument(&selected.binding.name, position);
        }
    }
    let Some(reordered) = reordered.into_iter().collect::<Option<Vec<_>>>() else {
        return name;
    };
    *args = reordered;
    for (argument, declared) in args.iter_mut().zip(&overload.argument_types) {
        let actual = scalar_type_inner(argument, schema, params, resolver)
            .ok()
            .flatten();
        if requires_cast(argument, actual.as_ref(), declared) {
            *argument = ScalarExpr::Cast {
                expr: Box::new(std::mem::replace(
                    argument,
                    ScalarExpr::Literal(Value::Null),
                )),
                ty: declared.sql_name(),
            };
        }
    }
    let dispatch = runtime_dispatch_name(&selected.binding);
    *binding = Some(selected.binding);
    dispatch.unwrap_or(name)
}

fn requires_cast(
    argument: &ScalarExpr,
    actual: Option<&ColumnType>,
    declared: &ColumnType,
) -> bool {
    if matches!(
        argument,
        ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
    ) {
        return true;
    }
    actual.is_none_or(|actual| {
        canonical_column_type_name(base_type(actual))
            != canonical_routine_type_name(&declared.sql_name())
    })
}

pub(crate) fn runtime_dispatch_name(binding: &FunctionBinding) -> Option<String> {
    let local = binding.name.rsplit('.').next()?;
    let arguments = binding.argument_types.as_slice();
    Some(
        match (local, arguments) {
            ("to_bin", [ty]) if ty == "integer" => TO_BIN_INT4_FUNCTION,
            ("to_bin", [ty]) if ty == "bigint" => TO_BIN_INT8_FUNCTION,
            ("to_hex", [ty]) if ty == "integer" => TO_HEX_INT4_FUNCTION,
            ("to_hex", [ty]) if ty == "bigint" => TO_HEX_INT8_FUNCTION,
            ("to_oct", [ty]) if ty == "integer" => TO_OCT_INT4_FUNCTION,
            ("to_oct", [ty]) if ty == "bigint" => TO_OCT_INT8_FUNCTION,
            ("random", [left, right]) if left == "integer" && right == "integer" => {
                RANDOM_INT4_FUNCTION
            }
            ("random", [left, right]) if left == "bigint" && right == "bigint" => {
                RANDOM_INT8_FUNCTION
            }
            ("random", [left, right]) if left == "numeric" && right == "numeric" => {
                RANDOM_NUMERIC_FUNCTION
            }
            _ => return None,
        }
        .into(),
    )
}

fn builtin_binding_is_volatile(binding: &FunctionBinding) -> bool {
    matches!(
        binding.name.rsplit('.').next(),
        Some("random" | "gen_random_uuid" | "uuidv4" | "uuidv7")
    )
}

fn default_argument(name: &str, position: usize) -> Option<ScalarExpr> {
    matches!(
        (name.rsplit('.').next(), position),
        (Some("json_strip_nulls" | "jsonb_strip_nulls"), 1)
    )
    .then_some(ScalarExpr::Literal(Value::Bool(false)))
}

fn argument_names(args: &[ScalarExpr]) -> Vec<Option<String>> {
    args.iter()
        .map(|argument| named_argument(argument).0)
        .collect()
}

fn effective_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument = named_argument_value(argument);
            super::effective_overload_argument_type(argument, argument_type.clone())
        })
        .collect()
}

fn unresolved_call_name(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> String {
    let arguments = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |name| {
                    format!("{name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{UNDEFINED_FUNCTION_MARKER}{name}({arguments})")
}

fn named_argument_value_owned(expression: ScalarExpr) -> ScalarExpr {
    if matches!(
        &expression,
        ScalarExpr::Func { name, args, .. }
            if name == uqa_sql::expr::NAMED_ARG_FUNCTION && args.len() == 2
    ) {
        let ScalarExpr::Func { mut args, .. } = expression else {
            unreachable!();
        };
        return args.pop().expect("named argument value follows its name");
    }
    expression
}

fn overloads(name: &str) -> Option<Vec<BuiltinFunctionOverload>> {
    let local = local_name(name)?;
    let overloads = match local.as_str() {
        "reverse" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Text),
            overload(&local, &[ColumnType::Bytea], ColumnType::Bytea),
        ],
        "md5" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Text),
            overload(&local, &[ColumnType::Bytea], ColumnType::Text),
        ],
        "crc32" | "crc32c" => {
            vec![overload(
                &local,
                &[ColumnType::Bytea],
                ColumnType::BigInteger,
            )]
        }
        "length" | "octet_length" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Integer),
            overload(&local, &[ColumnType::Bpchar], ColumnType::Integer),
            overload(&local, &[ColumnType::Bytea], ColumnType::Integer),
        ],
        "char_length" | "character_length" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Integer),
            overload(&local, &[ColumnType::Bpchar], ColumnType::Integer),
        ],
        "bit_length" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Integer),
            overload(&local, &[ColumnType::Bytea], ColumnType::Integer),
        ],
        "gamma" | "lgamma" => vec![overload(
            &local,
            &[ColumnType::DoublePrecision],
            ColumnType::DoublePrecision,
        )],
        "json_strip_nulls" => vec![overload_with_names_and_defaults(
            &local,
            &[ColumnType::Json, ColumnType::Boolean],
            &["target", "strip_in_arrays"],
            1,
            ColumnType::Json,
        )],
        "jsonb_strip_nulls" => vec![overload_with_names_and_defaults(
            &local,
            &[ColumnType::JsonB, ColumnType::Boolean],
            &["target", "strip_in_arrays"],
            1,
            ColumnType::JsonB,
        )],
        "to_bin" | "to_hex" | "to_oct" => vec![
            overload(&local, &[ColumnType::Integer], ColumnType::Text),
            overload(&local, &[ColumnType::BigInteger], ColumnType::Text),
        ],
        "random" => vec![
            overload(&local, &[], ColumnType::DoublePrecision),
            overload_with_names(
                &local,
                &[ColumnType::Integer, ColumnType::Integer],
                &["min", "max"],
                ColumnType::Integer,
            ),
            overload_with_names(
                &local,
                &[ColumnType::BigInteger, ColumnType::BigInteger],
                &["min", "max"],
                ColumnType::BigInteger,
            ),
            overload_with_names(
                &local,
                &[numeric_type(), numeric_type()],
                &["min", "max"],
                numeric_type(),
            ),
        ],
        "uuid_extract_timestamp" => vec![overload(
            &local,
            &[ColumnType::Uuid],
            ColumnType::TimestampTz,
        )],
        "uuid_extract_version" => vec![overload(
            &local,
            &[ColumnType::Uuid],
            ColumnType::SmallInteger,
        )],
        "gen_random_uuid" | "uuidv4" => vec![overload(&local, &[], ColumnType::Uuid)],
        "uuidv7" => vec![
            overload(&local, &[], ColumnType::Uuid),
            overload_with_names(
                &local,
                &[ColumnType::Interval],
                &["shift"],
                ColumnType::Uuid,
            ),
        ],
        "casefold" => vec![overload(&local, &[ColumnType::Text], ColumnType::Text)],
        _ => return None,
    };
    Some(overloads)
}

fn local_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if lower.contains('.') && !lower.starts_with("pg_catalog.") {
        return None;
    }
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    matches!(
        local,
        "reverse"
            | "md5"
            | "crc32"
            | "crc32c"
            | "length"
            | "char_length"
            | "character_length"
            | "octet_length"
            | "bit_length"
            | "gamma"
            | "lgamma"
            | "json_strip_nulls"
            | "jsonb_strip_nulls"
            | "to_bin"
            | "to_hex"
            | "to_oct"
            | "random"
            | "uuid_extract_timestamp"
            | "uuid_extract_version"
            | "gen_random_uuid"
            | "uuidv4"
            | "uuidv7"
            | "casefold"
    )
    .then(|| local.to_string())
}

fn overload(
    name: &str,
    argument_types: &[ColumnType],
    return_type: ColumnType,
) -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        name: format!("pg_catalog.{name}"),
        argument_names: vec![None; argument_types.len()],
        argument_types: argument_types.to_vec(),
        default_arguments: 0,
        return_type,
    }
}

fn overload_with_names(
    name: &str,
    argument_types: &[ColumnType],
    argument_names: &[&str],
    return_type: ColumnType,
) -> BuiltinFunctionOverload {
    overload_with_names_and_defaults(name, argument_types, argument_names, 0, return_type)
}

fn overload_with_names_and_defaults(
    name: &str,
    argument_types: &[ColumnType],
    argument_names: &[&str],
    default_arguments: usize,
    return_type: ColumnType,
) -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        name: format!("pg_catalog.{name}"),
        argument_names: argument_names
            .iter()
            .map(|name| Some((*name).into()))
            .collect(),
        argument_types: argument_types.to_vec(),
        default_arguments,
        return_type,
    }
}

fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}
