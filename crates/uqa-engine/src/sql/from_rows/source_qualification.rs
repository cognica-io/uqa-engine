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
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let columns = operator.schema().to_vec();
    qualify_source_operator_with_columns(operator, &columns, qualifier, prune, &[])
}

pub(in crate::sql) fn qualify_source_operator_with_columns<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    source_columns: &[String],
    qualifier: &str,
    prune: Option<&ColumnPrune>,
    aliases: &[String],
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
    Box::new(
        uqa_execution::ColumnSelection::with_identities(operator, mapping)
            .rebinding_lock_origins(qualifier),
    )
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
) -> Vec<String> {
    let lower = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    if lower == "unnest" {
        let width = output_width.max(1);
        let default_column = if width == 1 {
            alias.unwrap_or(output_name)
        } else {
            output_name
        };
        return (0..width)
            .map(|position| {
                column_aliases
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| default_column.to_string())
            })
            .collect();
    }
    if column_aliases.is_empty() {
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
            _ => vec![scalar_table_function_default_column(
                &lower,
                output_name,
                alias,
                column_aliases,
            )],
        }
    } else {
        column_aliases.to_vec()
    }
}

pub(in crate::sql) fn multi_unnest_internal_columns(width: usize) -> Vec<String> {
    (0..width)
        .map(|position| format!("\0uqa.unnest.{position}"))
        .collect()
}

pub(in crate::sql) fn table_function_column_types(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    declared_types: &[String],
    columns: &[String],
    input_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Vec<Option<ColumnType>> {
    let align = |types: Vec<Option<ColumnType>>| {
        if types.len() == columns.len() {
            types
        } else if let [ty] = types.as_slice() {
            vec![ty.clone(); columns.len()]
        } else {
            vec![None; columns.len()]
        }
    };
    if !declared_types.is_empty() {
        return align(
            declared_types
                .iter()
                .map(|ty| ColumnType::from_sql_name(ty).ok())
                .collect(),
        );
    }

    let normalized = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    let argument_type = |position: usize| {
        args.get(position)
            .and_then(|argument| uqa_execution::scalar_type(argument, input_schema, params).ok())
            .flatten()
    };
    let types = match normalized.as_str() {
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
        "regexp_split_to_table" | "string_to_table" | "json_object_keys" | "jsonb_object_keys" => {
            vec![Some(ColumnType::Text)]
        }
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
    };
    align(types)
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
