//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualification, view output, and table-function schemas.

use super::{
    execute_query_plan_output, qualifier_filter, restore_cte_names, save_and_remove_cte_names,
    BTreeSet, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator, QualifierFilters,
    QueryOutput, QueryOutputMode, QueryPlan, QueryRows, ResultRow, SQLError, SQLParam, Value,
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

pub(in crate::sql) use uqa_sql::semantics::join_alias_columns;

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

pub(in crate::sql) use uqa_sql::semantics::{
    apply_table_function_aliases, resolve_user_table_function,
    scalar_table_function_default_column, table_function_column_types, table_function_empty_schema,
    validate_table_function_alias_count, validate_table_function_column_definition,
    TableFunctionTypeRequest,
};
