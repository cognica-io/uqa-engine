//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualification, view output, and table-function schemas.

use super::{
    execute_query_plan_output, qualifier_filter, restore_cte_names, save_and_remove_cte_names,
    BTreeSet, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator, QualifierFilters,
    QueryOutput, QueryOutputMode, QueryPlan, QueryRows, ResultRow, SQLError, SQLParam, ScalarExpr,
    Value,
};
use crate::engine_user_functions::{
    routine_returns_anonymous_record, RoutineResolution, SQLUserFunction,
};
use std::sync::Arc;
use uqa_execution::{BuiltinFunctionOverload, FunctionTypeResolver, RowSchema};
use uqa_sql::ast::{
    ColumnType, FunctionBinding, FunctionParamMode, FunctionReturns, RoutineInvocationBinding,
};

pub(in crate::sql) struct ResolvedUserTableFunction {
    pub(in crate::sql) function: Arc<SQLUserFunction>,
    pub(in crate::sql) binding: FunctionBinding,
}

pub(in crate::sql) fn user_function_output_columns_for(
    function: &SQLUserFunction,
) -> Option<Vec<String>> {
    let outputs = function.def.output_params();
    if outputs.is_empty() {
        return None;
    }
    Some(
        outputs
            .iter()
            .enumerate()
            .map(|(position, parameter)| {
                if parameter.name.is_empty() {
                    format!("column{}", position + 1)
                } else {
                    parameter.name.clone()
                }
            })
            .collect(),
    )
}

fn validate_user_table_function_column_definition(
    function: &SQLUserFunction,
    declared_types: &[String],
) -> Result<(), SQLError> {
    let returns_anonymous_record = routine_returns_anonymous_record(&function.def);
    if declared_types.is_empty() {
        if returns_anonymous_record {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "a column definition list is required for functions returning \"record\""
                    .into(),
            });
        }
        return Ok(());
    }
    if returns_anonymous_record {
        return Ok(());
    }
    if function.def.output_params().len() > 1 {
        return Err(redundant_out_column_definition_error());
    }
    Err(SQLError::Routine {
        sqlstate: "42601".into(),
        message: "a column definition list is only allowed for functions returning \"record\""
            .into(),
    })
}

pub(in crate::sql) fn validate_table_function_column_definition(
    name: &str,
    binding: Option<&FunctionBinding>,
    user_function: Option<&SQLUserFunction>,
    declared_types: &[String],
) -> Result<(), SQLError> {
    if let Some(function) = user_function {
        return validate_user_table_function_column_definition(function, declared_types);
    }
    if declared_types.is_empty() || binding.is_some_and(|binding| !binding.builtin) {
        return Ok(());
    }
    let builtin = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    if matches!(
        builtin.as_str(),
        "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "pg_get_sequence_data"
            | "pg_sequence_parameters"
    ) {
        return Err(redundant_out_column_definition_error());
    }
    Ok(())
}

fn redundant_out_column_definition_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "a column definition list is redundant for a function with OUT parameters".into(),
    }
}

pub(in crate::sql) fn resolve_user_table_function(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ResolvedUserTableFunction>, SQLError> {
    let Some(binding) = resolve_table_function_binding(
        routines,
        name,
        binding,
        args,
        input_schema,
        params,
        resolver,
    )?
    else {
        return Ok(None);
    };
    if binding.builtin {
        return Ok(None);
    }
    let (argument_names, argument_types, explicit_variadic) =
        uqa_execution::function_call_argument_signature(
            args,
            input_schema,
            params,
            Some(resolver),
        )?;
    let Some(matched) = routines.resolve_static_sql_function_match(
        name,
        Some(&binding),
        &argument_names,
        &argument_types,
        explicit_variadic,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(ResolvedUserTableFunction {
        binding: matched.binding(),
        function: matched.function,
    }))
}

pub(in crate::sql) fn resolve_table_function_binding(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    input_schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<FunctionBinding>, SQLError> {
    if let Some(binding) = binding {
        return Ok(Some(binding.clone()));
    }
    let identity = name.to_ascii_lowercase();
    let builtin = crate::sql::builtin_function_dispatch_name(&identity);
    let (argument_names, argument_types, explicit_variadic) =
        uqa_execution::function_call_argument_signature(
            args,
            input_schema,
            params,
            Some(resolver),
        )?;
    let builtins = builtin_table_function_overloads(&builtin, &argument_types);
    if !builtins.is_empty() || has_builtin_table_function_overloads(&builtin) {
        return routines
            .resolve_table_function_overload_with_builtins(
                name,
                None,
                &argument_names,
                &argument_types,
                explicit_variadic,
                &builtins,
            )
            .map(|resolved| resolved.map(|resolved| resolved.binding));
    }
    let builtin_surface = is_builtin_table_function(&builtin)
        || crate::operator_tree_bridge::is_operator_join_table_function(&builtin)
        || routines.has_registered_table_function(&identity);
    if routines.lookup_visible_sql_functions(name)?.is_none() {
        return Ok(None);
    }
    match routines.resolve_static_sql_function_match(
        name,
        None,
        &argument_names,
        &argument_types,
        explicit_variadic,
    ) {
        Ok(Some(function)) => Ok(Some(function.binding())),
        Ok(None) => Ok(None),
        Err(error) if builtin_surface && error.sqlstate() == Some("42883") => Ok(None),
        Err(error) => Err(error),
    }
}

fn has_builtin_table_function_overloads(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "pg_listening_channels"
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
            | "pg_get_sequence_data"
            | "pg_sequence_parameters"
    )
}

fn builtin_table_function_overloads(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Vec<BuiltinFunctionOverload> {
    let canonical_name = format!("pg_catalog.{name}");
    let overload = |argument_types: Vec<ColumnType>,
                    default_arguments: usize,
                    return_type: ColumnType| BuiltinFunctionOverload {
        name: canonical_name.clone(),
        argument_names: vec![None; argument_types.len()],
        argument_types,
        default_arguments,
        return_type,
    };
    match name {
        "pg_listening_channels" => vec![overload(Vec::new(), 0, ColumnType::Text)],
        "generate_series" => vec![
            overload(
                vec![
                    ColumnType::Integer,
                    ColumnType::Integer,
                    ColumnType::Integer,
                ],
                1,
                ColumnType::Integer,
            ),
            overload(
                vec![
                    ColumnType::BigInteger,
                    ColumnType::BigInteger,
                    ColumnType::BigInteger,
                ],
                1,
                ColumnType::BigInteger,
            ),
        ],
        "unnest" => {
            let [Some(ColumnType::Array(element))] = argument_types else {
                return Vec::new();
            };
            vec![overload(vec![ColumnType::AnyArray], 0, (**element).clone())]
        }
        "regexp_split_to_table" | "string_to_table" => vec![overload(
            vec![ColumnType::Text, ColumnType::Text],
            0,
            ColumnType::Text,
        )],
        "json_array_elements" => vec![overload(vec![ColumnType::Json], 0, ColumnType::Json)],
        "jsonb_array_elements" => vec![overload(vec![ColumnType::JsonB], 0, ColumnType::JsonB)],
        "json_array_elements_text" | "json_object_keys" => {
            vec![overload(vec![ColumnType::Json], 0, ColumnType::Text)]
        }
        "jsonb_array_elements_text" | "jsonb_object_keys" => {
            vec![overload(vec![ColumnType::JsonB], 0, ColumnType::Text)]
        }
        "json_each" | "json_each_text" => {
            vec![overload(vec![ColumnType::Json], 0, ColumnType::Record)]
        }
        "jsonb_each" | "jsonb_each_text" => {
            vec![overload(vec![ColumnType::JsonB], 0, ColumnType::Record)]
        }
        "pg_get_sequence_data" => vec![overload(vec![ColumnType::Regclass], 0, ColumnType::Record)],
        "pg_sequence_parameters" => {
            vec![overload(vec![ColumnType::Oid], 0, ColumnType::Record)]
        }
        _ => Vec::new(),
    }
}

pub(in crate::sql) fn is_builtin_table_function(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "pg_listening_channels"
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
            | "pg_get_sequence_data"
            | "pg_sequence_parameters"
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

pub(in crate::sql) fn query_output_shared(
    output: QueryOutput,
    label: &str,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(format!(
            "{label} execution returned in-memory rows at an internal streaming boundary"
        )));
    };
    Ok(rows)
}

pub(in crate::sql) fn execute_view_plan_output_with_parent_cache(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    local_cte_names: &BTreeSet<String>,
) -> Result<QueryOutput, SQLError> {
    let saved = save_and_remove_cte_names(ctes, local_cte_names);
    let result =
        execute_query_plan_output(engine, plan, params, ctes, QueryOutputMode::SharedSpill);
    restore_cte_names(ctes, saved);
    result
}

pub(in crate::sql) fn qualify_source_operator<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    qualifier: &str,
    prune: Option<&ColumnPrune>,
    rebind_lock_origins: bool,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let columns = operator.schema().to_vec();
    qualify_source_operator_with_columns(
        operator,
        &columns,
        qualifier,
        prune,
        &[],
        rebind_lock_origins,
    )
}

pub(in crate::sql) fn qualify_source_operator_with_columns<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    source_columns: &[String],
    qualifier: &str,
    prune: Option<&ColumnPrune>,
    aliases: &[String],
    rebind_lock_origins: bool,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let mapping = source_columns
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let source_base = operator.row_schema().public_name(index).unwrap_or(source);
            let column = aliases.get(index).map_or(source_base, String::as_str);
            if !qualifier.is_empty()
                && prune
                    .and_then(|prune| prune.get(qualifier))
                    .is_some_and(|wanted| !wanted.contains(column))
            {
                return None;
            }
            let identity = if qualifier.is_empty() {
                uqa_execution::ColumnIdentity::unqualified(column)
            } else {
                uqa_execution::ColumnIdentity::qualified(qualifier, column)
            };
            Some((column.to_string(), identity, index))
        })
        .collect();
    let selection = uqa_execution::ColumnSelection::with_identities(operator, mapping)
        .rebinding_score_sources(qualifier);
    if rebind_lock_origins {
        Box::new(selection.rebinding_lock_origins(qualifier))
    } else {
        Box::new(selection.discarding_lock_origins())
    }
}

fn join_alias_columns(
    schema: &uqa_execution::RowSchema,
    alias: &str,
    column_aliases: &[String],
) -> Result<Vec<String>, SQLError> {
    let available = schema.len();
    let specified = column_aliases.len();
    if specified > available {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!(
                "join expression \"{alias}\" has {available} columns available but {specified} columns specified"
            ),
        });
    }
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            column_aliases
                .get(position)
                .cloned()
                .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
        })
        .collect())
}

pub(in crate::sql) fn alias_join_schema(
    schema: &uqa_execution::RowSchema,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<uqa_execution::RowSchema, SQLError> {
    let Some(alias) = alias else {
        if column_aliases.is_empty() {
            return Ok(schema.clone());
        }
        return Err(SQLError::Internal(
            "JOIN column aliases exist without a relation alias".into(),
        ));
    };
    let columns = join_alias_columns(schema, alias, column_aliases)?;
    Ok(uqa_execution::RowSchema::with_qualified_types(
        alias,
        columns,
        schema.column_types().to_vec(),
    ))
}

pub(in crate::sql) fn alias_join_operator<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    let Some(alias) = alias else {
        if column_aliases.is_empty() {
            return Ok(operator);
        }
        return Err(SQLError::Internal(
            "JOIN column aliases exist without a relation alias".into(),
        ));
    };
    let columns = join_alias_columns(operator.row_schema(), alias, column_aliases)?;
    let mapping = columns
        .into_iter()
        .enumerate()
        .map(|(position, column)| {
            (
                column.clone(),
                uqa_execution::ColumnIdentity::qualified(alias, column),
                position,
            )
        })
        .collect();
    Ok(Box::new(
        uqa_execution::ColumnSelection::with_fresh_identities(operator, mapping)
            .rebinding_score_sources(alias),
    ))
}

pub(in crate::sql) fn attach_qualifier_filter<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let Some(predicate) = qualifier_filter(filters, qualifier) else {
        return operator;
    };
    Box::new(uqa_execution::Filter::with_evaluator(
        operator,
        predicate,
        EngineExpressionEvaluator::shared(engine, params, ctes),
    ))
}

pub(in crate::sql) fn null_row_for_schema(schema: &[String]) -> ResultRow {
    schema
        .iter()
        .map(|column| (column.clone(), Value::Null))
        .collect()
}

pub(in crate::sql) fn table_function_empty_schema(
    name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    output_width: usize,
    ordinality: bool,
) -> Vec<String> {
    let lower = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    let columns = if lower == "unnest" {
        let width = output_width.max(1);
        let default_column = if width == 1 {
            alias.unwrap_or(output_name)
        } else {
            output_name
        };
        vec![default_column.to_string(); width]
    } else {
        match lower.as_str() {
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                vec!["key".into(), "value".into()]
            }
            "pg_get_sequence_data" => vec!["last_value".into(), "is_called".into()],
            "pg_sequence_parameters" => vec![
                "start_value".into(),
                "minimum_value".into(),
                "maximum_value".into(),
                "increment".into(),
                "cycle_option".into(),
                "cache_size".into(),
                "data_type".into(),
            ],
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec!["_doc_id".into(), "_score".into()],
            "rpq" => vec!["vertex_id".into()],
            "list_analyzers" => vec!["analyzer_name".into()],
            "fts_index_stats" => vec![
                "table_name".into(),
                "field".into(),
                "analyzer".into(),
                "posting_count".into(),
                "doc_length_count".into(),
                "indexed_doc_count".into(),
                "term_count".into(),
                "total_field_length".into(),
            ],
            "text_similarity_join"
            | "vector_similarity_join"
            | "graph_join"
            | "hybrid_join"
            | "cross_paradigm_join" => {
                vec!["left_doc_id".into(), "right_doc_id".into(), "_score".into()]
            }
            "generate_series"
            | "pg_listening_channels"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_object_keys"
            | "jsonb_object_keys" => vec![scalar_table_function_default_column(
                &lower,
                output_name,
                alias,
                &[],
            )],
            _ => {
                let minimum_width = 1;
                let aliased_value_width = if ordinality && column_aliases.len() > minimum_width {
                    column_aliases.len() - 1
                } else {
                    column_aliases.len()
                };
                vec![
                    scalar_table_function_default_column(&lower, output_name, alias, &[]);
                    minimum_width.max(aliased_value_width)
                ]
            }
        }
    };
    apply_table_function_aliases(columns, column_aliases, ordinality)
}

pub(in crate::sql) fn apply_table_function_aliases(
    mut columns: Vec<String>,
    column_aliases: &[String],
    ordinality: bool,
) -> Vec<String> {
    if ordinality {
        columns.push("ordinality".into());
    }
    for (column, alias) in columns.iter_mut().zip(column_aliases) {
        column.clone_from(alias);
    }
    columns
}

pub(in crate::sql) fn validate_table_function_alias_count(
    table_alias: &str,
    available: usize,
    specified: usize,
) -> Result<(), SQLError> {
    if specified <= available {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42P10".into(),
        message: format!(
            "table \"{table_alias}\" has {available} columns available but {specified} columns specified"
        ),
    })
}

pub(in crate::sql) struct TableFunctionTypeRequest<'a> {
    pub(in crate::sql) name: &'a str,
    pub(in crate::sql) args: &'a [ScalarExpr],
    pub(in crate::sql) user_function: Option<&'a SQLUserFunction>,
    pub(in crate::sql) user_invocation: Option<&'a RoutineInvocationBinding>,
    pub(in crate::sql) declared_types: &'a [String],
    pub(in crate::sql) columns: &'a [String],
    pub(in crate::sql) ordinality: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(in crate::sql) fn table_function_column_types(
    routines: &dyn RoutineResolution,
    request: TableFunctionTypeRequest<'_>,
    input_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Vec<Option<ColumnType>> {
    let TableFunctionTypeRequest {
        name,
        args,
        user_function,
        user_invocation,
        declared_types,
        columns,
        ordinality,
    } = request;
    let value_columns = if ordinality {
        columns
            .get(..columns.len().saturating_sub(1))
            .unwrap_or(&[])
    } else {
        columns
    };
    let align = |types: Vec<Option<ColumnType>>| {
        if types.len() == value_columns.len() {
            types
        } else if let [ty] = types.as_slice() {
            vec![ty.clone(); value_columns.len()]
        } else {
            vec![None; value_columns.len()]
        }
    };
    let mut types = if !declared_types.is_empty() {
        align(
            declared_types
                .iter()
                .map(|ty| ColumnType::from_sql_name(ty).ok())
                .collect(),
        )
    } else if let Some(function) = user_function {
        align(user_function_column_types(
            routines,
            function,
            user_invocation,
        ))
    } else {
        let normalized = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
        let argument_type = |position: usize| {
            args.get(position)
                .and_then(|argument| {
                    uqa_execution::scalar_type_with_resolver(
                        argument,
                        input_schema,
                        params,
                        resolver,
                    )
                    .ok()
                })
                .flatten()
        };
        align(match normalized.as_str() {
            "pg_listening_channels" => vec![Some(ColumnType::Text)],
            "generate_series" => vec![argument_type(0)],
            "unnest" => args
                .iter()
                .map(|argument| {
                    uqa_execution::scalar_type_with_resolver(
                        argument,
                        input_schema,
                        params,
                        resolver,
                    )
                    .ok()
                    .flatten()
                    .and_then(|ty| match ty {
                        ColumnType::Array(element) => Some(*element),
                        _ => None,
                    })
                })
                .collect(),
            "regexp_split_to_table"
            | "string_to_table"
            | "json_object_keys"
            | "jsonb_object_keys" => vec![Some(ColumnType::Text)],
            "json_array_elements" => vec![Some(ColumnType::Json)],
            "jsonb_array_elements" => vec![Some(ColumnType::JsonB)],
            "json_array_elements_text" | "jsonb_array_elements_text" => {
                vec![Some(ColumnType::Text)]
            }
            "json_each" => vec![Some(ColumnType::Text), Some(ColumnType::Json)],
            "jsonb_each" => vec![Some(ColumnType::Text), Some(ColumnType::JsonB)],
            "json_each_text" | "jsonb_each_text" => {
                vec![Some(ColumnType::Text), Some(ColumnType::Text)]
            }
            "pg_get_sequence_data" => {
                vec![Some(ColumnType::BigInteger), Some(ColumnType::Boolean)]
            }
            "pg_sequence_parameters" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::Boolean),
                Some(ColumnType::BigInteger),
                Some(ColumnType::Oid),
            ],
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::DoublePrecision),
            ],
            "rpq" => vec![Some(ColumnType::BigInteger)],
            "text_similarity_join"
            | "vector_similarity_join"
            | "graph_join"
            | "hybrid_join"
            | "cross_paradigm_join" => vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::BigInteger),
                Some(ColumnType::DoublePrecision),
            ],
            _ => user_table_function_column_types(
                routines,
                name,
                args,
                input_schema,
                params,
                resolver,
            ),
        })
    };
    if ordinality {
        types.push(Some(ColumnType::BigInteger));
    }
    types
}

fn user_table_function_column_types(
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    input_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Vec<Option<ColumnType>> {
    let Ok((argument_names, argument_types, explicit_variadic)) =
        uqa_execution::function_call_argument_signature(args, input_schema, params, Some(resolver))
    else {
        return Vec::new();
    };
    let Ok(Some(matched)) = routines.resolve_static_sql_function_match(
        name,
        None,
        &argument_names,
        &argument_types,
        explicit_variadic,
    ) else {
        return Vec::new();
    };
    user_function_column_types(routines, &matched.function, Some(&matched.invocation))
}

fn user_function_column_types(
    routines: &dyn RoutineResolution,
    function: &SQLUserFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Vec<Option<ColumnType>> {
    let outputs = function
        .def
        .params
        .iter()
        .enumerate()
        .filter(|(_, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
        })
        .collect::<Vec<_>>();
    if !outputs.is_empty() {
        return outputs
            .into_iter()
            .map(|(index, parameter)| {
                let type_name = invocation
                    .and_then(|binding| binding.parameter_types.get(index))
                    .unwrap_or(&parameter.type_name);
                resolve_table_function_column_type(routines, type_name)
            })
            .collect();
    }
    match &function.def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            let type_name = invocation
                .and_then(|binding| binding.return_type.as_ref())
                .unwrap_or(type_name);
            vec![resolve_table_function_column_type(routines, type_name)]
        }
        FunctionReturns::None | FunctionReturns::Table => Vec::new(),
    }
}

fn resolve_table_function_column_type(
    routines: &dyn RoutineResolution,
    type_name: &str,
) -> Option<ColumnType> {
    routines
        .resolve_type_name(type_name)
        .ok()
        .flatten()
        .or_else(|| ColumnType::from_sql_name(type_name).ok())
}

pub(in crate::sql) fn is_json_array_table_function(name: &str) -> bool {
    matches!(
        name,
        "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
    )
}

pub(in crate::sql) fn scalar_table_function_default_column(
    normalized_name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> String {
    column_aliases.first().cloned().unwrap_or_else(|| {
        if is_json_array_table_function(normalized_name) {
            "value".into()
        } else {
            alias.unwrap_or(output_name).to_string()
        }
    })
}
