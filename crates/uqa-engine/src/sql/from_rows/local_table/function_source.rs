//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical assembly for table-function sources and `ROWS FROM` groups.

use super::{
    attach_qualifier_filter, build_table_function_row_stream_with_row,
    qualify_source_operator_with_columns, resolve_user_table_function, table_function_column_types,
    validate_table_function_alias_count, validate_table_function_column_definition, ColumnPrune,
    CteScope, Engine, QualifierFilters, RowsFromOperator, SQLError, SQLParam, ScopedEngineHook,
    SourceEvalContext, SourcePlan, TableFunctionCall, TableFunctionTypeRequest,
};
use uqa_execution::PhysicalOperator;
use uqa_planner::TableFunctionPlan;
use uqa_sql::ast::FunctionBinding;

struct FunctionSource<'a> {
    name: &'a str,
    binding: Option<&'a FunctionBinding>,
    output_name: &'a str,
    relation: Option<&'a str>,
    args: &'a [uqa_planner::ScalarExpr],
    alias: Option<&'a str>,
    column_aliases: &'a [String],
    ordinality: bool,
    column_types: &'a [String],
}

impl<'a> FunctionSource<'a> {
    fn group_member(function: &'a TableFunctionPlan) -> Self {
        Self {
            name: &function.name,
            binding: function.binding.as_ref(),
            output_name: &function.output_name,
            relation: function.relation.as_deref(),
            args: &function.args,
            alias: None,
            column_aliases: &function.column_aliases,
            ordinality: false,
            column_types: &function.column_types,
        }
    }
}

/// Build the physical operator for a single table-function source.
pub(super) fn build_function_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SourcePlan::Function {
        name,
        binding,
        output_name,
        relation,
        args,
        alias,
        column_aliases,
        ordinality,
        column_types,
    } = from
    else {
        unreachable!("table-function source builder called for a different source kind")
    };
    build_table_function_operator(
        engine,
        FunctionSource {
            name,
            binding: binding.as_ref(),
            output_name,
            relation: relation.as_deref(),
            args,
            alias: alias.as_deref(),
            column_aliases,
            ordinality: *ordinality,
            column_types,
        },
        params,
        ctes,
        prune,
        filters,
    )
}

/// Build a `ROWS FROM` group by resolving every member independently, then
/// streaming their rows through `PostgreSQL`'s zip-longest/NULL-pad contract.
pub(super) fn build_function_group_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SourcePlan::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ordinality,
    } = from
    else {
        unreachable!("function-group source builder called for a different source kind")
    };
    let first = functions
        .first()
        .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
    let mut members = Vec::with_capacity(functions.len());
    for function in functions {
        members.push(build_table_function_operator(
            engine,
            FunctionSource::group_member(function),
            params,
            ctes,
            None,
            None,
        )?);
    }
    let operator: Box<dyn PhysicalOperator + 'a> =
        Box::new(RowsFromOperator::new(members, *ordinality));
    let source_columns = operator
        .row_schema()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            operator
                .row_schema()
                .public_name(position)
                .unwrap_or(column)
                .to_string()
        })
        .collect::<Vec<_>>();
    let qualifier = alias.as_deref().unwrap_or(&first.output_name);
    validate_table_function_alias_count(qualifier, source_columns.len(), column_aliases.len())?;
    let operator = qualify_source_operator_with_columns(
        operator,
        &source_columns,
        qualifier,
        prune,
        column_aliases,
        ctes.lock_identities.emit,
    );
    Ok(attach_qualifier_filter(
        operator, qualifier, filters, engine, params, ctes,
    ))
}

fn build_table_function_operator<'a>(
    engine: &'a Engine,
    source: FunctionSource<'_>,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let outer_row = ctes.row_lock_outer_row().cloned();
    let hook = ScopedEngineHook::new(engine, ctes);
    let input_schema = outer_row
        .as_ref()
        .map_or_else(uqa_execution::RowSchema::default, |row| row.schema.clone());
    let resolved = resolve_user_table_function(
        engine,
        source.name,
        source.binding,
        source.args,
        &input_schema,
        params,
        &hook,
    )?;
    validate_table_function_column_definition(
        source.name,
        source.binding,
        resolved.as_ref().map(|resolved| resolved.function.as_ref()),
        source.column_types,
    )?;
    let context = SourceEvalContext::new(engine, params, &hook, &hook, &ctes.scalar_subqueries);
    let call = TableFunctionCall {
        name: source.name,
        binding: resolved
            .as_ref()
            .map(|resolved| &resolved.binding)
            .or(source.binding),
        output_name: source.output_name,
        relation: source.relation,
        args: source.args,
        alias: source.alias,
        column_aliases: source.column_aliases,
        ordinality: source.ordinality,
        column_types: source.column_types,
    };
    let output = build_table_function_row_stream_with_row(&context, call, outer_row.as_ref())?;
    let columns = output.columns;
    let rows = output.rows;
    validate_table_function_alias_count(
        source.alias.unwrap_or(source.output_name),
        columns.len(),
        source.column_aliases.len(),
    )?;
    let types = table_function_column_types(
        engine,
        TableFunctionTypeRequest {
            name: source.name,
            args: source.args,
            user_function: resolved.as_ref().map(|resolved| resolved.function.as_ref()),
            user_invocation: resolved
                .as_ref()
                .and_then(|resolved| resolved.binding.invocation.as_deref()),
            declared_types: source.column_types,
            columns: &columns,
            ordinality: source.ordinality,
        },
        &input_schema,
        params,
        &hook,
    );
    let schema = uqa_execution::RowSchema::with_types(columns.clone(), types);
    let operator: Box<dyn PhysicalOperator + 'a> =
        Box::new(uqa_execution::PhysicalRowIteratorScan::new(schema, rows));
    let source_columns = columns;
    let qualifier = source.alias.unwrap_or(source.output_name);
    let operator = qualify_source_operator_with_columns(
        operator,
        &source_columns,
        qualifier,
        prune,
        &[],
        ctes.lock_identities.emit,
    );
    Ok(attach_qualifier_filter(
        operator, qualifier, filters, engine, params, ctes,
    ))
}
