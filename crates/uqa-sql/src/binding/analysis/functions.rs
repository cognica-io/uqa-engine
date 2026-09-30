//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static column and routine lookup used by query analysis.

use super::super::{ColumnType, SQLError, SQLParam, ScalarExpr};
use crate::ast::FunctionBinding;
use crate::routines::RoutineResolution;
use crate::type_resolution::builtin_function_type_with_resolver;
use crate::{FunctionTypeResolver, RowSchema, ScalarOrder};
use std::collections::BTreeSet;

pub(super) fn validate_unqualified_column(
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    column: &str,
) -> Result<(), SQLError> {
    if column == "_score" {
        if schema.score_source_is_ambiguous(None) {
            return Err(SQLError::AmbiguousColumn(column.to_string()));
        }
        if schema.score_source_column(None).is_some() {
            return Ok(());
        }
    }
    if schema.column_is_ambiguous(column) {
        return Err(SQLError::AmbiguousColumn(column.to_string()));
    }
    if is_pseudo_column(column)
        && !schema.has_unqualified_column(column)
        && pseudo_column_qualifiers(schema, column).len() > 1
    {
        return Err(SQLError::AmbiguousColumn(column.to_string()));
    }
    if schema.has_unqualified_column(column) {
        return Ok(());
    }
    if schema.has_qualifier(column) {
        return Ok(());
    }
    if schema.columns_are_open(None) {
        return Ok(());
    }
    if let Some(fallback) = fallback {
        if fallback.column_is_ambiguous(column) {
            return Err(SQLError::AmbiguousColumn(column.to_string()));
        }
        if fallback.has_unqualified_column(column) {
            return Ok(());
        }
        if fallback.has_qualifier(column) {
            return Ok(());
        }
        if fallback.columns_are_open(None) {
            return Ok(());
        }
    }
    Err(SQLError::UnknownColumn(column.to_string()))
}

pub(super) fn validate_qualified_column(
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    qualifier: &str,
    column: &str,
) -> Result<(), SQLError> {
    for candidate in std::iter::once(schema).chain(fallback) {
        if !candidate.has_qualifier(qualifier) {
            continue;
        }
        if candidate.qualified_column_is_ambiguous(qualifier, column) {
            return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
        }
        if candidate.has_qualified_column(qualifier, column)
            || candidate.columns_are_open(Some(qualifier))
        {
            return Ok(());
        }
        return Err(SQLError::unknown_qualified_column(qualifier, column));
    }
    Err(SQLError::UnknownTable(qualifier.to_string()))
}

pub(super) fn single_pseudo_column_qualifier(schema: &RowSchema) -> Option<String> {
    let mut qualifiers = schema_qualifiers(schema).into_iter().filter(|qualifier| {
        schema.has_qualified_column(qualifier, "_doc_id")
            && schema.has_qualified_column(qualifier, "_score")
            && schema.has_qualified_column(qualifier, "tableoid")
    });
    let qualifier = qualifiers.next()?;
    qualifiers.next().is_none().then_some(qualifier)
}

fn pseudo_column_qualifiers(schema: &RowSchema, column: &str) -> BTreeSet<String> {
    schema_qualifiers(schema)
        .into_iter()
        .filter(|qualifier| schema.has_qualified_column(qualifier, column))
        .collect()
}

fn schema_qualifiers(schema: &RowSchema) -> BTreeSet<String> {
    schema
        .identities()
        .iter()
        .filter_map(|identity| identity.qualifier())
        .chain(
            schema
                .typed_virtual_identities()
                .filter_map(|(identity, _)| identity.qualifier()),
        )
        .map(str::to_string)
        .collect()
}

fn is_pseudo_column(column: &str) -> bool {
    matches!(column, "_doc_id" | "_score" | "tableoid" | "xmin")
}

pub(super) struct ScalarFunctionValidation<'a> {
    pub(super) name: &'a str,
    pub(super) binding: Option<&'a FunctionBinding>,
    pub(super) args: &'a [ScalarExpr],
    pub(super) order_by: &'a [ScalarOrder],
    pub(super) expression: &'a ScalarExpr,
    pub(super) schema: &'a RowSchema,
    pub(super) params: &'a [SQLParam],
    pub(super) resolver: &'a dyn FunctionTypeResolver,
}

pub(super) fn validate_scalar_function(
    routines: &dyn RoutineResolution,
    validation: ScalarFunctionValidation<'_>,
) -> Result<(), SQLError> {
    let ScalarFunctionValidation {
        name,
        binding,
        args,
        order_by,
        expression,
        schema,
        params,
        resolver,
    } = validation;
    let identity = name.to_ascii_lowercase();
    let lower = crate::semantics::builtin_function_dispatch_name(&identity);
    if binding.and_then(|binding| binding.dispatch).is_some() {
        crate::scalar_type_with_resolver(expression, schema, params, resolver)?;
        return Ok(());
    }
    crate::scalar_call_arguments(args)?;
    if routines.has_registered_scalar_function(&identity) {
        return Ok(());
    }
    if validate_fixed_builtin(name, binding, args, schema, params, resolver)? {
        return Ok(());
    }
    if let Some(valid) =
        sequence_function_signature_matches(&lower, args, schema, params, resolver)?
    {
        if valid {
            return Ok(());
        }
        if resolve_sql_function(routines, name, binding, args, schema, params, resolver)?.is_some()
        {
            return Ok(());
        }
        return Err(undefined_function(name, args, schema, params, resolver));
    }
    if matches!(
        lower.as_str(),
        "uuid_extract_version" | "uuid_extract_timestamp"
    ) {
        return validate_uuid_extraction_function(name, args, schema, params, resolver);
    }
    if binding.is_none() && matches!(lower.as_str(), "array_sort" | "array_reverse") {
        crate::scalar_type_with_resolver(expression, schema, params, resolver)?;
        return Ok(());
    }
    if crate::registry::is_registered(&lower)
        || crate::semantics::is_builtin_aggregate(expression)
        || routines.has_registered_aggregate_function(&identity)
        || builtin_scalar_function(&lower, args.len())
    {
        return Ok(());
    }
    if resolve_sql_function(routines, name, binding, args, schema, params, resolver)?.is_some() {
        return Ok(());
    }
    if builtin_function_type_with_resolver(&lower, args, order_by, schema, params, resolver)?
        .is_some()
    {
        return Ok(());
    }
    Err(undefined_function(name, args, schema, params, resolver))
}

fn validate_fixed_builtin(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    if !crate::is_fixed_builtin(name) {
        return Ok(false);
    }
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
    crate::resolve_fixed_builtin_call(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        Some(resolver),
    )
    .map(|resolved| resolved.is_some())
}

fn sequence_function_signature_matches(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<bool>, SQLError> {
    let parameter_types: &[&str] = match name {
        "nextval" | "currval" => &["regclass"],
        "lastval" => &[],
        "setval" if args.len() == 2 => &["regclass", "int8"],
        "setval" => &["regclass", "int8", "bool"],
        _ => return Ok(None),
    };
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
    if explicit_variadic {
        return Ok(Some(false));
    }
    let parameters = parameter_types
        .iter()
        .map(|type_name| crate::FunctionParameterDescriptor {
            name: None,
            type_name: (*type_name).into(),
            has_default: false,
        })
        .collect::<Vec<_>>();
    Ok(Some(
        crate::match_function_signature(&parameters, &argument_names, &argument_types).is_some(),
    ))
}

fn validate_uuid_extraction_function(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    let call_arguments = crate::scalar_call_arguments(args)?;
    let valid = if let [argument] = call_arguments.as_slice() {
        argument.name.is_none()
            && !argument.explicit_variadic
            && crate::common_context_expression_type(
                argument.value,
                schema,
                params,
                Some(resolver),
            )?
            .as_ref()
            .is_none_or(uuid_compatible_type)
    } else {
        false
    };
    if valid {
        Ok(())
    } else {
        Err(undefined_function(name, args, schema, params, resolver))
    }
}

fn uuid_compatible_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Uuid => true,
        ColumnType::Domain { base, .. } => uuid_compatible_type(base),
        _ => false,
    }
}

/// What `func_get_detail` found for a window call's name and arguments.
enum WindowCallKind {
    WindowFunction,
    /// An ordered-set or hypothetical-set aggregate.
    OrderedSetAggregate,
    Aggregate,
    Ordinary,
}

/// Validate a window call as `ParseFuncOrColumn` does: resolve the function by its arguments, which include a `WITHIN GROUP` call's ordering expressions, then reject what that kind of function cannot take. `call` holds the arguments, whether the call has `FILTER`, and its other aggregate modifiers.
pub(super) fn validate_window_function(
    routines: &dyn RoutineResolution,
    name: &str,
    (args, filtered, modifiers): (&[ScalarExpr], bool, crate::ast::WindowCallModifiers),
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    let kind = window_call_kind(routines, name, args, schema, params, resolver)?
        .ok_or_else(|| undefined_function(name, args, schema, params, resolver))?;
    let wrong_object = |message: String| SQLError::Routine {
        sqlstate: "42809".into(),
        message,
    };
    let unsupported = |message: &str| SQLError::Routine {
        sqlstate: "0A000".into(),
        message: message.into(),
    };
    match kind {
        WindowCallKind::Ordinary => {
            let modifier = if modifiers.distinct {
                "DISTINCT"
            } else if modifiers.within_group {
                "WITHIN GROUP"
            } else if modifiers.ordered {
                "ORDER BY"
            } else if filtered {
                "FILTER"
            } else {
                return Err(wrong_object(format!(
                    "OVER specified, but {name} is not a window function nor an aggregate function"
                )));
            };
            Err(wrong_object(format!(
                "{modifier} specified, but {name} is not an aggregate function"
            )))
        }
        WindowCallKind::OrderedSetAggregate => Err(if modifiers.within_group {
            unsupported(&format!(
                "OVER is not supported for ordered-set aggregate {name}"
            ))
        } else {
            wrong_object(format!(
                "WITHIN GROUP is required for ordered-set aggregate {name}"
            ))
        }),
        WindowCallKind::Aggregate | WindowCallKind::WindowFunction => {
            let aggregate = matches!(kind, WindowCallKind::Aggregate);
            if modifiers.within_group {
                return Err(wrong_object(if aggregate {
                    format!(
                        "{name} is not an ordered-set aggregate, so it cannot have WITHIN GROUP"
                    )
                } else {
                    format!("window function {name} cannot have WITHIN GROUP")
                }));
            }
            if modifiers.distinct {
                return Err(unsupported(
                    "DISTINCT is not implemented for window functions",
                ));
            }
            if aggregate && args.is_empty() {
                return Err(wrong_object(format!(
                    "{name}(*) must be used to call a parameterless aggregate function"
                )));
            }
            if modifiers.ordered {
                return Err(unsupported(
                    "aggregate ORDER BY is not implemented for window functions",
                ));
            }
            if !aggregate && filtered {
                return Err(unsupported(
                    "FILTER is not implemented for non-aggregate window functions",
                ));
            }
            Ok(())
        }
    }
}

/// `func_get_detail` for a window call: the built-in window functions, the ordered-set aggregates with their direct and ordered arguments, the built-in and registered aggregates, and then any other function the arguments resolve; `None` when nothing matches.
fn window_call_kind(
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<WindowCallKind>, SQLError> {
    let lower = crate::semantics::builtin_function_dispatch_name(name);
    let kind = match (lower.as_str(), args.len()) {
        ("row_number" | "rank" | "dense_rank" | "percent_rank" | "cume_dist", 0)
        | ("lag" | "lead", 1..=3)
        | ("first_value" | "last_value" | "ntile", 1)
        | ("nth_value", 2) => Some(WindowCallKind::WindowFunction),
        ("percentile_cont" | "percentile_disc", 2)
        | ("mode", 1)
        | ("rank" | "dense_rank" | "percent_rank" | "cume_dist", 1..) => {
            Some(WindowCallKind::OrderedSetAggregate)
        }
        (
            "count" | "sum" | "avg" | "min" | "max" | "array_agg" | "json_agg" | "jsonb_agg"
            | "bool_and" | "bool_or" | "stddev" | "stddev_samp" | "stddev_pop" | "variance"
            | "var_samp" | "var_pop",
            1,
        )
        | ("count", 0)
        | ("string_agg" | "json_object_agg" | "jsonb_object_agg", 2) => {
            Some(WindowCallKind::Aggregate)
        }
        _ if routines.has_registered_aggregate_function(name) => Some(WindowCallKind::Aggregate),
        _ => None,
    };
    if kind.is_some() {
        return Ok(kind);
    }
    // The built-in window and ordered-set functions have no other signatures; only a user routine can take these arguments.
    if matches!(
        lower.as_str(),
        "row_number"
            | "rank"
            | "dense_rank"
            | "percent_rank"
            | "cume_dist"
            | "ntile"
            | "lag"
            | "lead"
            | "first_value"
            | "last_value"
            | "nth_value"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
    ) {
        return Ok(
            resolve_sql_function(routines, name, None, args, schema, params, resolver)?
                .map(|_| WindowCallKind::Ordinary),
        );
    }
    let call = ScalarExpr::Func {
        name: name.to_string(),
        binding: None,
        args: args.to_vec(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    // A built-in aggregate reaching here has no overload for these arguments.
    Ok((!crate::semantics::is_builtin_aggregate(&call)
        && validate_scalar_function(
            routines,
            ScalarFunctionValidation {
                name,
                binding: None,
                args,
                order_by: &[],
                expression: &call,
                schema,
                params,
                resolver,
            },
        )
        .is_ok())
    .then_some(WindowCallKind::Ordinary))
}

pub(super) fn validate_table_function(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<crate::semantics::ResolvedUserTableFunction>, SQLError> {
    if let Some(resolved) = crate::semantics::resolve_user_table_function(
        routines, name, binding, args, input, params, resolver,
    )? {
        return Ok(Some(resolved));
    }
    let identity = name.to_ascii_lowercase();
    let lower = crate::semantics::builtin_function_dispatch_name(&identity);
    if binding.is_none_or(|binding| binding.builtin)
        && (crate::semantics::is_builtin_table_function(&lower)
            || crate::registry::is_operator_join_table_function(&lower)
            || routines.has_registered_table_function(&identity))
    {
        return Ok(None);
    }
    Err(undefined_function(name, args, input, params, resolver))
}

fn resolve_sql_function(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<std::sync::Arc<crate::routines::SQLUserFunction>>, SQLError> {
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
    if binding.is_none() && routines.lookup_visible_sql_functions(name)?.is_none() {
        return Ok(None);
    }
    routines.resolve_static_sql_function(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
    )
}

fn named_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
    crate::scalar_call_argument(expression).map_or((None, expression), |argument| {
        (argument.name.map(str::to_string), argument.value)
    })
}

fn builtin_scalar_function(name: &str, argument_count: usize) -> bool {
    if crate::expr::builtin_scalar_function_strictness(name, argument_count).is_some() {
        return true;
    }
    matches!(
        (name, argument_count),
        (
            "pi" | "random" | "now" | "current_timestamp" | "current_date",
            0
        ) | (
            "clock_timestamp"
                | "statement_timestamp"
                | "transaction_timestamp"
                | "current_time"
                | "localtime"
                | "localtimestamp"
                | "timeofday"
                | "current_database"
                | "current_catalog"
                | "current_schema"
                | "current_user"
                | "session_user"
                | "gen_random_uuid"
                | "uuidv4"
                | "merge_action",
            0
        ) | ("uuidv7", 0..=1)
            | ("setseed", 1)
            | ("crc32" | "crc32c", 1)
            | ("div", 2)
            | ("generate_series", 2..=3)
            | ("unnest", 1..)
            | ("array_sample", 2)
    )
}

fn undefined_function(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> SQLError {
    let signature = args
        .iter()
        .map(|argument| {
            let (argument_name, value) = named_argument(argument);
            let ty = crate::common_context_expression_type(value, schema, params, Some(resolver))
                .ok()
                .and_then(|ty| {
                    crate::effective_overload_argument_type_with_params(value, ty, params)
                })
                .map_or_else(|| "unknown".to_string(), |ty| ty.regtype_name());
            argument_name.map_or(ty.clone(), |name| format!("{name} => {ty}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::undefined_function_call(&format!("{name}({signature})"))
}
