//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualification, view output, and table-function schemas.

use super::{
    execute_query_plan_output, is_score_provenance_column, qualified_key, qualifier_filter,
    restore_cte_names, save_and_remove_cte_names, BTreeSet, ColumnPrune, CteScope, Engine,
    EngineExpressionEvaluator, QualifierFilters, QueryOutput, QueryOutputMode, QueryPlan,
    QueryRows, ResultRow, SQLError, SQLParam, Value,
};

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
                source
                    .rsplit_once('.')
                    .map_or(source.as_str(), |(_, column)| column)
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
            let output = if qualifier.is_empty() {
                column.to_string()
            } else {
                qualified_key(qualifier, column)
            };
            Some((output, source.clone()))
        })
        .collect();
    Box::new(uqa_execution::ColumnSelection::with_mapping(
        operator, mapping,
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
    alias: Option<&str>,
    column_aliases: &[String],
) -> Vec<String> {
    let lower = crate::sql::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    let columns = if column_aliases.is_empty() {
        match lower.as_str() {
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                vec!["key".into(), "value".into()]
            }
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec!["_doc_id".into(), "_score".into()],
            "rpq" => vec!["vertex_id".into()],
            _ => vec![scalar_table_function_default_column(
                &lower,
                alias,
                column_aliases,
            )],
        }
    } else {
        column_aliases.to_vec()
    };
    match alias {
        Some(alias) => columns
            .into_iter()
            .map(|column| qualified_key(alias, &column))
            .collect(),
        None => columns,
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
    alias: Option<&str>,
    column_aliases: &[String],
) -> String {
    column_aliases.first().cloned().unwrap_or_else(|| {
        if is_json_array_table_function(normalized_name) {
            "value".into()
        } else {
            alias.unwrap_or(normalized_name).to_string()
        }
    })
}
