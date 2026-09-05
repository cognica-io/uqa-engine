//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One registry and binding path for implemented fixed-signature `PostgreSQL` built-ins.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding, FunctionDispatch};
use uqa_sql::{SQLError, SQLParam};

use crate::{scalar_call_arguments, RowSchema, ScalarExpr};

use super::common::base_type;
use super::functions::{named_argument, named_argument_value};
use super::{
    builtin_binding_matches, canonical_column_type_name, canonical_routine_type_name,
    match_builtin_function_overload, resolve_local_builtin_overload, scalar_type_inner,
    BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload,
};

#[doc(hidden)]
#[must_use]
pub fn is_function(name: &str) -> bool {
    overloads(name).is_some()
}

/// Fixed-signature call metadata needed by generated-column binding without exposing the built-in registry itself.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFixedBuiltinCall {
    pub selected: ResolvedFunctionOverload,
    pub builtin_argument_positions: Option<Vec<usize>>,
    pub builtin_non_immutable: bool,
}

/// Resolve an implemented fixed-signature built-in together with visible SQL routine overloads. `None` means `name` is outside the fixed registry.
#[doc(hidden)]
pub fn resolve_fixed_builtin_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFixedBuiltinCall>, SQLError> {
    let Some(builtins) = overloads(name) else {
        return Ok(None);
    };
    let selected = resolve_overload(
        name,
        binding,
        argument_names,
        argument_types,
        explicit_variadic,
        resolver,
    )?;
    let (builtin_argument_positions, builtin_non_immutable) = if selected.binding.builtin {
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
            builtin_binding_is_non_immutable(&selected.binding),
        )
    } else {
        (None, false)
    };
    Ok(Some(ResolvedFixedBuiltinCall {
        selected,
        builtin_argument_positions,
        builtin_non_immutable,
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
    explicit_variadic: bool,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let argument_names = argument_names(args);
    let argument_types = effective_argument_types(args, argument_types, params);
    resolve_overload(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        resolver,
    )
    .map(|overload| Some(overload.return_type))
}

pub(super) fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
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
            explicit_variadic,
            &builtins,
        )? {
            return Ok(selected);
        }
    }
    if explicit_variadic && argument_names.iter().any(Option::is_some) {
        return Err(super::function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
    }
    resolve_local_builtin_overload(name, binding, argument_names, argument_types, &builtins)
}

pub(super) fn selected_argument_targets(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Option<Vec<Option<ColumnType>>> {
    let argument_names = vec![None; argument_types.len()];
    let selected =
        resolve_overload(name, None, &argument_names, argument_types, false, None).ok()?;
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
    if binding.is_none() && resolver.is_some_and(|resolver| resolver.has_untyped_function(&name)) {
        return name;
    }
    let Some(builtins) = overloads(&name) else {
        return name;
    };
    let Ok(call_arguments) = scalar_call_arguments(args) else {
        return name;
    };
    let explicit_variadic = call_arguments
        .iter()
        .any(|argument| argument.explicit_variadic);
    let Ok(argument_types) = call_arguments
        .iter()
        .map(|argument| scalar_type_inner(argument.value, schema, params, resolver))
        .collect::<Result<Vec<_>, _>>()
    else {
        return name;
    };
    let names = argument_names(args);
    let argument_types = effective_argument_types(args, &argument_types, params);
    let selected = resolve_overload(
        &name,
        binding.as_ref(),
        &names,
        &argument_types,
        explicit_variadic,
        resolver,
    );
    let mut selected = match selected {
        Ok(selected) => selected,
        Err(error) if error.sqlstate() == Some("42883") => {
            let signature = unresolved_call_signature(&name, &names, &argument_types);
            *binding = Some(FunctionBinding::undefined_function(name.clone(), signature));
            return name;
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
    if !reorder_arguments(
        args,
        &matched.argument_positions,
        overload.argument_types.len(),
        &selected.binding.name,
    ) {
        return name;
    }
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
    selected.binding.dispatch = runtime_dispatch(&selected.binding);
    *binding = Some(selected.binding);
    name
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

pub(crate) fn runtime_dispatch(binding: &FunctionBinding) -> Option<FunctionDispatch> {
    let local = binding.name.rsplit('.').next()?;
    let arguments = binding.argument_types.as_slice();
    Some(match (local, arguments) {
        ("to_bin", [ty]) if ty == "integer" => FunctionDispatch::ToBinInt4,
        ("to_bin", [ty]) if ty == "bigint" => FunctionDispatch::ToBinInt8,
        ("to_hex", [ty]) if ty == "integer" => FunctionDispatch::ToHexInt4,
        ("to_hex", [ty]) if ty == "bigint" => FunctionDispatch::ToHexInt8,
        ("to_oct", [ty]) if ty == "integer" => FunctionDispatch::ToOctInt4,
        ("to_oct", [ty]) if ty == "bigint" => FunctionDispatch::ToOctInt8,
        ("random", [left, right]) if left == "integer" && right == "integer" => {
            FunctionDispatch::RandomInt4Range
        }
        ("random", [left, right]) if left == "bigint" && right == "bigint" => {
            FunctionDispatch::RandomInt8Range
        }
        ("random", [left, right]) if left == "numeric" && right == "numeric" => {
            FunctionDispatch::RandomNumericRange
        }
        _ => return None,
    })
}

fn builtin_binding_is_non_immutable(binding: &FunctionBinding) -> bool {
    matches!(
        binding.name.rsplit('.').next(),
        Some(
            "random"
                | "gen_random_uuid"
                | "uuidv4"
                | "uuidv7"
                | "pg_get_expr"
                | "pg_get_partkeydef"
                | "pg_backend_pid"
                | "pg_listening_channels"
                | "pg_notify"
                | "pg_notification_queue_usage"
                | "pg_get_serial_sequence"
                | "pg_get_sequence_data"
                | "pg_sequence_last_value"
                | "pg_sequence_parameters"
                | "pg_get_triggerdef"
                | "pg_get_ruledef"
                | "pg_get_viewdef"
                | "pg_has_role"
                | "has_table_privilege"
                | "has_column_privilege"
                | "has_database_privilege"
                | "has_schema_privilege"
                | "has_sequence_privilege"
                | "to_regproc"
                | "to_regprocedure"
                | "to_regclass"
                | "to_regnamespace"
                | "to_regrole"
                | "to_regtype"
        )
    )
}

fn default_argument(name: &str, position: usize) -> Option<ScalarExpr> {
    matches!(
        (name.rsplit('.').next(), position),
        (Some("json_strip_nulls" | "jsonb_strip_nulls"), 1)
    )
    .then_some(ScalarExpr::Literal(Value::Bool(false)))
}

fn reorder_arguments(
    args: &mut Vec<ScalarExpr>,
    argument_positions: &[usize],
    parameter_count: usize,
    binding_name: &str,
) -> bool {
    if args.len() != argument_positions.len() {
        return false;
    }
    let mut supplied = vec![false; parameter_count];
    for &position in argument_positions {
        let Some(slot) = supplied.get_mut(position) else {
            return false;
        };
        if std::mem::replace(slot, true) {
            return false;
        }
    }
    let mut reordered = (0..parameter_count)
        .map(|position| default_argument(binding_name, position))
        .collect::<Vec<_>>();
    if supplied
        .iter()
        .zip(&reordered)
        .any(|(supplied, default)| !supplied && default.is_none())
    {
        return false;
    }
    for (argument, &position) in std::mem::take(args).into_iter().zip(argument_positions) {
        reordered[position] = Some(named_argument_value_owned(argument));
    }
    *args = reordered
        .into_iter()
        .map(|argument| argument.expect("every fixed built-in argument was prevalidated"))
        .collect();
    true
}

fn argument_names(args: &[ScalarExpr]) -> Vec<Option<String>> {
    args.iter()
        .map(|argument| named_argument(argument).0)
        .collect()
}

fn effective_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    params: &[SQLParam],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument = named_argument_value(argument);
            super::effective_overload_argument_type_with_params(
                argument,
                argument_type.clone(),
                params,
            )
        })
        .collect()
}

fn unresolved_call_signature(
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
    format!("{name}({arguments})")
}

fn named_argument_value_owned(expression: ScalarExpr) -> ScalarExpr {
    if matches!(
        &expression,
        ScalarExpr::Func { binding, args, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(FunctionDispatch::NamedArgument)
                && args.len() == 2
    ) {
        let ScalarExpr::Func { mut args, .. } = expression else {
            unreachable!();
        };
        return args.pop().expect("named argument value follows its name");
    }
    expression
}

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
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
        "to_regproc" => vec![overload(&local, &[ColumnType::Text], ColumnType::Regproc)],
        "to_regprocedure" => vec![overload(
            &local,
            &[ColumnType::Text],
            ColumnType::Regprocedure,
        )],
        "to_regclass" => vec![overload(&local, &[ColumnType::Text], ColumnType::Regclass)],
        "to_regnamespace" => vec![overload(
            &local,
            &[ColumnType::Text],
            ColumnType::Regnamespace,
        )],
        "to_regrole" => vec![overload(&local, &[ColumnType::Text], ColumnType::Regrole)],
        "to_regtype" => vec![overload(&local, &[ColumnType::Text], ColumnType::Regtype)],
        "pg_get_expr" => vec![
            overload(
                &local,
                &[ColumnType::PgNodeTree, ColumnType::Oid],
                ColumnType::Text,
            ),
            overload(
                &local,
                &[ColumnType::PgNodeTree, ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "pg_get_partkeydef" => vec![overload(&local, &[ColumnType::Oid], ColumnType::Text)],
        "pg_backend_pid" => vec![overload(&local, &[], ColumnType::Integer)],
        "pg_listening_channels" => vec![overload(&local, &[], ColumnType::Text)],
        "pg_notify" => vec![overload(
            &local,
            &[ColumnType::Text, ColumnType::Text],
            ColumnType::Void,
        )],
        "pg_notification_queue_usage" => {
            vec![overload(&local, &[], ColumnType::DoublePrecision)]
        }
        "pg_get_serial_sequence" => vec![overload(
            &local,
            &[ColumnType::Text, ColumnType::Text],
            ColumnType::Text,
        )],
        "pg_get_sequence_data" => vec![overload(
            &local,
            &[ColumnType::Regclass],
            ColumnType::Record,
        )],
        "pg_sequence_last_value" => vec![overload(
            &local,
            &[ColumnType::Regclass],
            ColumnType::BigInteger,
        )],
        "pg_sequence_parameters" => vec![overload(&local, &[ColumnType::Oid], ColumnType::Record)],
        "pg_get_triggerdef" | "pg_get_ruledef" => vec![
            overload(&local, &[ColumnType::Oid], ColumnType::Text),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
        ],
        "pg_get_viewdef" => vec![
            overload(&local, &[ColumnType::Text], ColumnType::Text),
            overload(&local, &[ColumnType::Oid], ColumnType::Text),
            overload(
                &local,
                &[ColumnType::Text, ColumnType::Boolean],
                ColumnType::Text,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Boolean],
                ColumnType::Text,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Integer],
                ColumnType::Text,
            ),
        ],
        "pg_has_role" => vec![
            overload(
                &local,
                &[ColumnType::Name, ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Name, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Name, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],
        "has_column_privilege" => vec![
            overload(
                &local,
                &[
                    ColumnType::Name,
                    ColumnType::Text,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Name,
                    ColumnType::Text,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Name,
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Name,
                    ColumnType::Oid,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Oid,
                    ColumnType::Oid,
                    ColumnType::Text,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[
                    ColumnType::Oid,
                    ColumnType::Oid,
                    ColumnType::SmallInteger,
                    ColumnType::Text,
                ],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Text, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Text, ColumnType::SmallInteger, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::SmallInteger, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],
        "has_table_privilege"
        | "has_database_privilege"
        | "has_schema_privilege"
        | "has_sequence_privilege" => vec![
            overload(
                &local,
                &[ColumnType::Name, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Name, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Text, ColumnType::Text],
                ColumnType::Boolean,
            ),
            overload(
                &local,
                &[ColumnType::Oid, ColumnType::Text],
                ColumnType::Boolean,
            ),
        ],
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
            | "to_regproc"
            | "to_regprocedure"
            | "to_regclass"
            | "to_regnamespace"
            | "to_regrole"
            | "to_regtype"
            | "pg_get_expr"
            | "pg_get_partkeydef"
            | "pg_backend_pid"
            | "pg_listening_channels"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "pg_get_serial_sequence"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
            | "pg_sequence_parameters"
            | "pg_get_triggerdef"
            | "pg_get_ruledef"
            | "pg_get_viewdef"
            | "pg_has_role"
            | "has_table_privilege"
            | "has_column_privilege"
            | "has_database_privilege"
            | "has_schema_privilege"
            | "has_sequence_privilege"
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

#[cfg(test)]
mod privilege_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_gate_accepts_only_supported_builtin_names() {
        assert!(is_function("PG_CATALOG.MD5"));
        assert!(!is_function("public.md5"));
        assert!(!is_function("not_a_builtin"));
    }

    #[test]
    fn failed_argument_reordering_preserves_original_arguments() {
        let original = vec![ScalarExpr::Literal(Value::Int(7))];
        let mut args = original.clone();

        assert!(!reorder_arguments(&mut args, &[1], 2, "pg_catalog.random"));
        assert_eq!(args, original);
    }
}
