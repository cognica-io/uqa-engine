//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical assembly for table-function sources and `ROWS FROM` groups.

use super::{
    attach_closed_qualifier_filter, qualify_source_operator_with_columns,
    resolve_user_table_function, table_function_column_types, validate_table_function_alias_count,
    validate_table_function_column_definition, ColumnPrune, CteScope, PhysicalOperator,
    QualifierFilters, RowsFromOperator, SQLError, SQLParam, SourceContext, SourcePlan,
    TableFunctionCall, TableFunctionTypeRequest,
};
use uqa_sql::{ast::FunctionBinding, plan::TableFunctionPlan};

#[cfg(test)]
#[path = "function_source/tests/runtime.rs"]
mod test_support;
#[cfg(test)]
mod tests;

mod prepared_call;
mod prepared_group;
use prepared_call::{deferred_function_rows, PreparedFunctionCall};
pub(super) use prepared_group::PreparedFunctionGroup;

struct FunctionSource<'a> {
    name: &'a str,
    binding: Option<&'a FunctionBinding>,
    output_name: &'a str,
    relations: Option<&'a uqa_sql::ast::OperatorJoinRelations>,
    args: &'a [uqa_sql::ScalarExpr],
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
            relations: function.relations.as_ref(),
            args: &function.args,
            alias: None,
            column_aliases: &function.column_aliases,
            ordinality: false,
            column_types: &function.column_types,
        }
    }
}

/// Build the physical operator for a single table-function source.
pub(super) fn build_function_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SourcePlan::Function {
        name,
        binding,
        output_name,
        relations,
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
        context,
        FunctionSource {
            name,
            binding: binding.as_ref(),
            output_name,
            relations: relations.as_ref(),
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
pub(super) fn build_function_group_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
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
    let mut members: Vec<Box<dyn PhysicalOperator + 'a>> = Vec::with_capacity(functions.len());
    for function in functions {
        let member = build_table_function_operator(
            context,
            FunctionSource::group_member(function),
            params,
            ctes,
            None,
            None,
        );
        match member {
            Ok(member) => members.push(member),
            Err(mut error) => {
                for member in &mut members {
                    if let Err(cleanup) = member.close() {
                        error = error.with_cleanup(
                            crate::physical::physical_exec_error(cleanup),
                            "close earlier ROWS FROM member after initialization failure",
                        );
                    }
                }
                return Err(error);
            }
        }
    }
    let mut operator: Box<dyn PhysicalOperator + 'a> =
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
    if let Err(error) =
        validate_table_function_alias_count(qualifier, source_columns.len(), column_aliases.len())
    {
        return Err(match operator.close() {
            Ok(()) => error,
            Err(cleanup) => error.with_cleanup(
                crate::physical::physical_exec_error(cleanup),
                "close ROWS FROM after alias validation failure",
            ),
        });
    }
    let operator = qualify_source_operator_with_columns(
        operator,
        &source_columns,
        qualifier,
        prune,
        column_aliases,
        ctes.lock_identities.emit,
    );
    Ok(attach_closed_qualifier_filter(
        operator, qualifier, filters, context, params, ctes,
    ))
}

fn build_table_function_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    source: FunctionSource<'_>,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let outer_row = ctes.row_lock_outer_row().cloned();
    let hook = context.relational.expression_scope(ctes.clone());
    let initialized = initialize_function_source(context, &source, params, &hook, outer_row)?;
    let operator = initialized.operator(&source, prune, ctes.lock_identities.emit);
    Ok(attach_closed_qualifier_filter(
        operator,
        source.alias.unwrap_or(source.output_name),
        filters,
        context,
        params,
        ctes,
    ))
}

struct InitializedFunctionSource<'a> {
    columns: Vec<String>,
    schema: crate::RowSchema,
    rows: FunctionRows<'a>,
    prepared: Option<std::sync::Arc<PreparedFunctionCall>>,
}

impl<'a> InitializedFunctionSource<'a> {
    fn operator(
        self,
        source: &FunctionSource<'_>,
        prune: Option<&ColumnPrune>,
        emit_lock_origins: bool,
    ) -> Box<dyn PhysicalOperator + 'a> {
        let operator = Box::new(crate::PhysicalRowIteratorScan::from_stream(
            self.schema,
            self.rows,
        ));
        qualify_source_operator_with_columns(
            operator,
            &self.columns,
            source.alias.unwrap_or(source.output_name),
            prune,
            &[],
            emit_lock_origins,
        )
    }
}

fn initialize_function_source<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    source: &FunctionSource<'_>,
    params: &'a [SQLParam],
    hook: &std::sync::Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    outer_row: Option<crate::OwnedPhysicalRow>,
) -> Result<InitializedFunctionSource<'a>, SQLError> {
    let input_schema = outer_row
        .as_ref()
        .map_or_else(crate::RowSchema::default, |row| row.schema.clone());
    let resolved = resolve_user_table_function(
        context.relational.catalog,
        source.name,
        source.binding,
        source.args,
        &input_schema,
        params,
        hook.as_ref(),
    )?;
    validate_table_function_column_definition(
        context.relational.catalog,
        uqa_sql::semantics::ColumnDefinitionRequest {
            name: source.name,
            binding: source.binding,
            user_function: resolved.as_ref().map(|resolved| resolved.function.as_ref()),
            user_invocation: resolved
                .as_ref()
                .and_then(|resolved| resolved.binding.invocation.as_deref()),
            args: source.args,
            declared_types: source.column_types,
            column_names: source.column_aliases,
        },
        &input_schema,
        params,
        hook.as_ref(),
    )?;
    let call = TableFunctionCall {
        name: source.name,
        binding: resolved
            .as_ref()
            .map(|resolved| &resolved.binding)
            .or(source.binding),
        output_name: source.output_name,
        relations: source.relations,
        args: source.args,
        function_states: None,
        routine_descriptions: None,
        alias: source.alias,
        column_aliases: source.column_aliases,
        ordinality: source.ordinality,
        column_types: source.column_types,
    };
    let (columns, mut rows, prepared) = function_rows(
        context,
        source,
        call,
        resolved.as_ref(),
        hook,
        params,
        outer_row,
    )?;
    if let Err(error) = validate_table_function_alias_count(
        source.alias.unwrap_or(source.output_name),
        columns.len(),
        source.column_aliases.len(),
    ) {
        return Err(rows.fail(error));
    }
    let types = table_function_column_types(
        context.relational.catalog,
        TableFunctionTypeRequest {
            name: source.name,
            binding: source.binding,
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
        hook.as_ref(),
    );
    let schema = crate::RowSchema::with_types(columns.clone(), types);
    Ok(InitializedFunctionSource {
        columns,
        schema,
        rows,
        prepared,
    })
}

type InitializedRows<'a> = (
    Vec<String>,
    FunctionRows<'a>,
    Option<std::sync::Arc<PreparedFunctionCall>>,
);

type FunctionRows<'a> = crate::SQLRowStream<'a, crate::PhysicalRow>;

/// The output columns and rows of a function in FROM. `PostgreSQL` checks EXECUTE when it initializes a function scan and calls the function only when the scan's first row is requested; a call whose columns only its result reveals runs at once.
fn function_rows<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    source: &FunctionSource<'_>,
    call: TableFunctionCall<'_>,
    resolved: Option<&uqa_sql::semantics::ResolvedUserTableFunction>,
    hook: &std::sync::Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    params: &'a [SQLParam],
    outer_row: Option<crate::OwnedPhysicalRow>,
) -> Result<InitializedRows<'a>, SQLError> {
    let deferred_columns = match resolved {
        Some(resolved) => {
            hook.require_user_function_execute(&resolved.function)?;
            Some(user_function_columns(
                context.relational.catalog,
                resolved,
                source,
            )?)
        }
        None => {
            let declared = context
                .relational
                .catalog
                .registered_table_function_columns(call.name);
            let columns = match declared {
                Some(columns) => Some(uqa_sql::semantics::apply_table_function_aliases(
                    columns,
                    call.column_aliases,
                    call.ordinality,
                )),
                None => crate::query::table_functions::deferred_table_function_columns(&call)
                    .transpose()?,
            };
            if let Some(binding) = call.binding.filter(|binding| binding.builtin) {
                if columns.is_some() {
                    hook.require_builtin_execute(binding)?;
                }
            }
            columns
        }
    };
    let input_schema = outer_row
        .as_ref()
        .map_or_else(crate::RowSchema::default, |row| row.schema.clone());
    require_argument_function_execute(
        context.relational.catalog,
        hook.as_ref(),
        source.args,
        &input_schema,
        params,
    )?;
    if let Some(columns) = deferred_columns {
        let bound = hook.routine_description_scope().is_some().then(|| {
            call.args
                .iter()
                .map(|argument| {
                    uqa_sql::bind_type_introspection_with_resolver(
                        argument.clone(),
                        &input_schema,
                        params,
                        hook.as_ref(),
                    )
                })
                .collect()
        });
        let prepared = std::sync::Arc::new(PreparedFunctionCall::new(&call, bound));
        let rows = deferred_function_rows(
            std::sync::Arc::clone(&prepared),
            std::sync::Arc::clone(hook),
            params,
            outer_row,
            columns.len(),
        );
        return Ok((columns, rows, Some(prepared)));
    }
    let output = hook.table_function_rows(call, params, outer_row.as_ref())?;
    let rows: FunctionRows<'a> = output.rows;
    Ok((output.columns, rows, None))
}

/// Initialize argument calls without evaluating them. Their retained routine identities must pass EXECUTE even when the scan is never fetched.
pub(super) fn require_argument_function_execute(
    catalog: &dyn uqa_sql::semantics::sets::SetFunctionCatalog,
    runtime: &dyn crate::query::set_projection::SetFunctionRuntime,
    arguments: &[uqa_sql::ScalarExpr],
    schema: &crate::RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for argument in arguments {
        argument.try_visit(&mut |expression| {
            if let uqa_sql::ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } = expression
            {
                if let Some(binding) = binding.as_ref().filter(|binding| binding.builtin) {
                    runtime.require_builtin_execute(binding)?;
                } else if let Some(resolved) = resolve_user_table_function(
                    catalog,
                    name,
                    binding.as_ref(),
                    args,
                    schema,
                    params,
                    runtime,
                )? {
                    runtime.require_user_function_execute(&resolved.function)?;
                }
            }
            Ok::<_, SQLError>(true)
        })?;
    }
    Ok(())
}

/// The output columns of a user function in FROM, including an ordinality column, as its declared result and the FROM item's aliases name them.
fn user_function_columns(
    catalog: &dyn uqa_sql::semantics::sets::SetFunctionCatalog,
    resolved: &uqa_sql::semantics::ResolvedUserTableFunction,
    source: &FunctionSource<'_>,
) -> Result<Vec<String>, SQLError> {
    uqa_sql::semantics::user_table_function_columns(
        catalog,
        &resolved.function,
        resolved.binding.invocation.as_deref(),
        source.output_name,
        source.alias,
        source.column_aliases,
        source.ordinality,
    )
}
