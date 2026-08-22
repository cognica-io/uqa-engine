//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualification, view output, and table-function schemas.

use super::{
    execute_query_plan_output, is_score_provenance_column, qualifier_filter, restore_cte_names,
    save_and_remove_cte_names, BTreeSet, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator,
    QualifierFilters, QueryOutput, QueryOutputMode, QueryPlan, QueryRows, ResultRow, SQLError,
    SQLParam, ScalarExpr, Value,
};
use uqa_sql::ast::{ColumnType, FunctionReturns};

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
            let source_base = if is_score_provenance_column(source) {
                source.as_str()
            } else {
                operator.row_schema().public_name(index).unwrap_or(source)
            };
            let column = aliases.get(index).map_or(source_base, String::as_str);
            if !is_score_provenance_column(column)
                && !qualifier.is_empty()
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
    let selection = uqa_execution::ColumnSelection::with_identities(operator, mapping);
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
        uqa_execution::ColumnSelection::with_fresh_identities(operator, mapping),
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
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec!["_doc_id".into(), "_score".into()],
            "rpq" => vec!["vertex_id".into()],
            "text_similarity_join"
            | "vector_similarity_join"
            | "graph_join"
            | "hybrid_join"
            | "cross_paradigm_join" => {
                vec!["left_doc_id".into(), "right_doc_id".into(), "_score".into()]
            }
            "generate_series"
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

pub(in crate::sql) fn multi_unnest_internal_columns(width: usize) -> Vec<String> {
    (0..width)
        .map(|position| format!("\0uqa.unnest.{position}"))
        .collect()
}

pub(in crate::sql) struct TableFunctionTypeRequest<'a> {
    pub(in crate::sql) name: &'a str,
    pub(in crate::sql) args: &'a [ScalarExpr],
    pub(in crate::sql) declared_types: &'a [String],
    pub(in crate::sql) columns: &'a [String],
    pub(in crate::sql) ordinality: bool,
}

pub(in crate::sql) fn table_function_column_types(
    engine: &Engine,
    request: TableFunctionTypeRequest<'_>,
    input_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Vec<Option<ColumnType>> {
    let TableFunctionTypeRequest {
        name,
        args,
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
    let mut types = if declared_types.is_empty() {
        let normalized = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
        let argument_type = |position: usize| {
            args.get(position)
                .and_then(|argument| {
                    uqa_execution::scalar_type(argument, input_schema, params).ok()
                })
                .flatten()
        };
        align(match normalized.as_str() {
            "generate_series" => vec![argument_type(0)],
            "unnest" => args
                .iter()
                .map(|argument| {
                    uqa_execution::scalar_type(argument, input_schema, params)
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
            _ => user_table_function_column_types(engine, name, args, input_schema, params),
        })
    } else {
        align(
            declared_types
                .iter()
                .map(|ty| ColumnType::from_sql_name(ty).ok())
                .collect(),
        )
    };
    if ordinality {
        types.push(Some(ColumnType::BigInteger));
    }
    types
}

fn user_table_function_column_types(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    input_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Vec<Option<ColumnType>> {
    let Some(overloads) = engine.lookup_sql_functions(name) else {
        return Vec::new();
    };
    let argument_types = args
        .iter()
        .map(|argument| {
            uqa_execution::scalar_type(argument, input_schema, params)
                .ok()
                .flatten()
                .map(|ty| crate::engine_user_functions::canonical_routine_type_name(&ty.sql_name()))
        })
        .collect::<Option<Vec<_>>>();
    let candidates = overloads
        .iter()
        .filter(|function| !function.def.is_procedure)
        .filter(|function| {
            argument_types.as_ref().is_none_or(|types| {
                crate::engine_user_functions::routine_signature_types(&function.def) == *types
            })
        })
        .collect::<Vec<_>>();
    let [function] = candidates.as_slice() else {
        return Vec::new();
    };
    let outputs = function.def.output_params();
    if !outputs.is_empty() {
        return outputs
            .iter()
            .map(|parameter| ColumnType::from_sql_name(&parameter.type_name).ok())
            .collect();
    }
    match &function.def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            vec![ColumnType::from_sql_name(type_name).ok()]
        }
        FunctionReturns::None | FunctionReturns::Table => Vec::new(),
    }
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
