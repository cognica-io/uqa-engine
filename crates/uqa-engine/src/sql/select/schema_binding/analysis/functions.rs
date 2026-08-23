//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static column and routine lookup used by query analysis.

use super::super::{ColumnType, Engine, SQLError, SQLParam, ScalarExpr};
use std::collections::BTreeSet;
use uqa_execution::type_resolution::builtin_function_type;
use uqa_execution::{RowSchema, ScalarOrder};
use uqa_sql::ast::FunctionBinding;

pub(super) fn validate_unqualified_column(
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    column: &str,
) -> Result<(), SQLError> {
    if schema.column_is_ambiguous(column) {
        return Err(SQLError::AmbiguousColumn(column.to_string()));
    }
    if is_pseudo_column(column) && pseudo_column_qualifiers(schema, column).len() > 1 {
        return Err(SQLError::AmbiguousColumn(column.to_string()));
    }
    if schema.has_unqualified_column(column) {
        return Ok(());
    }
    if let Some(fallback) = fallback {
        if fallback.column_is_ambiguous(column) {
            return Err(SQLError::AmbiguousColumn(column.to_string()));
        }
        if fallback.has_unqualified_column(column) {
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
        if candidate.has_qualified_column(qualifier, column) {
            return Ok(());
        }
        return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
    }
    Err(SQLError::UnknownTable(qualifier.to_string()))
}

pub(super) fn is_semantic_all_argument(function: &str, argument: &ScalarExpr) -> bool {
    matches!(argument, ScalarExpr::Column(column) if column == "_all")
        && uqa_sql::registry::is_registered(&crate::sql::builtin_function_dispatch_name(function))
}

pub(super) fn single_pseudo_column_qualifier(schema: &RowSchema) -> Option<String> {
    let mut qualifiers = schema
        .identities()
        .iter()
        .filter_map(|identity| identity.qualifier())
        .filter(|qualifier| {
            schema.has_qualified_column(qualifier, "_doc_id")
                && schema.has_qualified_column(qualifier, "_score")
        })
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter();
    let qualifier = qualifiers.next()?;
    qualifiers.next().is_none().then_some(qualifier)
}

fn pseudo_column_qualifiers(schema: &RowSchema, column: &str) -> BTreeSet<String> {
    schema
        .identities()
        .iter()
        .filter_map(|identity| identity.qualifier())
        .filter(|qualifier| schema.has_qualified_column(qualifier, column))
        .map(str::to_string)
        .collect()
}

fn is_pseudo_column(column: &str) -> bool {
    matches!(column, "_doc_id" | "_score")
}

pub(super) struct ScalarFunctionValidation<'a> {
    pub(super) name: &'a str,
    pub(super) binding: Option<&'a FunctionBinding>,
    pub(super) args: &'a [ScalarExpr],
    pub(super) order_by: &'a [ScalarOrder],
    pub(super) expression: &'a ScalarExpr,
    pub(super) schema: &'a RowSchema,
    pub(super) params: &'a [SQLParam],
}

pub(super) fn validate_scalar_function(
    engine: &Engine,
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
    } = validation;
    let identity = name.to_ascii_lowercase();
    let lower = crate::sql::builtin_function_dispatch_name(&identity);
    if matches!(
        lower.as_str(),
        "uuid_extract_version" | "uuid_extract_timestamp"
    ) {
        return validate_uuid_extraction_function(engine, name, args, schema, params);
    }
    if lower == uqa_sql::expr::NAMED_ARG_FUNCTION
        || uqa_sql::registry::is_registered(&lower)
        || crate::sql::aggregates::is_aggregate(engine, expression)
        || engine.has_registered_scalar_function(&identity)
        || engine.has_registered_aggregate_function(&identity)
        || builtin_scalar_function(&lower, args.len())
    {
        return Ok(());
    }
    if resolve_sql_function(engine, name, binding, args, schema, params)?.is_some() {
        return Ok(());
    }
    if builtin_function_type(&lower, args, order_by, schema, params)?.is_some() {
        return Ok(());
    }
    Err(undefined_function(name, args, schema, params))
}

fn validate_uuid_extraction_function(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let valid = if let [argument] = args {
        let (argument_name, value) = named_argument(argument);
        argument_name.is_none()
            && uqa_execution::common_context_expression_type(value, schema, params, Some(engine))?
                .as_ref()
                .is_none_or(uuid_compatible_type)
    } else {
        false
    };
    if valid {
        Ok(())
    } else {
        Err(undefined_function(name, args, schema, params))
    }
}

fn uuid_compatible_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Uuid => true,
        ColumnType::Domain { base, .. } => uuid_compatible_type(base),
        _ => false,
    }
}

pub(super) fn validate_window_function(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let lower = crate::sql::builtin_function_dispatch_name(name);
    if matches!(
        (lower.as_str(), args.len()),
        ("row_number" | "rank" | "dense_rank", 0)
            | ("lag" | "lead", 1..=3)
            | ("first_value" | "last_value", 1)
            | ("nth_value", 2)
            | ("ntile", 1)
    ) || engine.has_registered_aggregate_function(name)
        || resolve_sql_function(engine, name, None, args, schema, params)?.is_some()
    {
        Ok(())
    } else {
        Err(undefined_function(name, args, schema, params))
    }
}

pub(super) fn validate_table_function(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    input: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let identity = name.to_ascii_lowercase();
    let lower = crate::sql::builtin_function_dispatch_name(&identity);
    if builtin_table_function(&lower)
        || crate::operator_tree_bridge::is_operator_join_table_function(&lower)
        || engine.has_registered_table_function(&identity)
        || resolve_sql_function(engine, name, None, args, input, params)?.is_some()
    {
        Ok(())
    } else {
        Err(undefined_function(name, args, input, params))
    }
}

fn resolve_sql_function(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<std::sync::Arc<crate::engine_user_functions::SQLUserFunction>>, SQLError> {
    if binding.is_none() && engine.lookup_sql_functions(name).is_none() {
        return Ok(None);
    }
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for argument in args {
        let (argument_name, value) = named_argument(argument);
        argument_names.push(argument_name);
        argument_types.push(uqa_execution::common_context_expression_type(
            value,
            schema,
            params,
            Some(engine),
        )?);
    }
    engine.resolve_static_sql_function(name, binding, &argument_names, &argument_types)
}

fn named_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return (None, expression);
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return (None, expression);
    }
    let name = args.first().and_then(|name| match name {
        ScalarExpr::Literal(uqa_core::Value::Str(name)) => Some(name.clone()),
        _ => None,
    });
    (name, args.get(1).unwrap_or(expression))
}

fn builtin_scalar_function(name: &str, argument_count: usize) -> bool {
    if uqa_sql::expr::builtin_scalar_function_strictness(name, argument_count).is_some() {
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
            | ("nextval" | "currval", 1)
            | ("setval", 2)
            | ("crc32" | "crc32c", 1)
            | ("div", 2)
            | ("generate_series", 2..=3)
            | ("unnest", 1..)
            | ("array_sample", 2)
    )
}

fn builtin_table_function(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "unnest"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_object_keys"
            | "jsonb_object_keys"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "create_analyzer"
            | "drop_analyzer"
            | "list_analyzers"
            | "fts_index_stats"
            | "set_table_analyzer"
            | "pagerank"
            | "graph_pagerank"
            | "hits"
            | "graph_hits"
            | "betweenness"
            | "graph_betweenness"
            | "graph_edges"
            | "rpq"
            | "cypher"
    )
}

fn undefined_function(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> SQLError {
    let signature = args
        .iter()
        .map(|argument| {
            let (argument_name, value) = named_argument(argument);
            let ty = uqa_execution::common_context_expression_type(value, schema, params, None)
                .ok()
                .flatten()
                .map_or_else(|| "unknown".to_string(), |ty| ty.sql_name());
            argument_name.map_or(ty.clone(), |name| format!("{name} => {ty}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}
