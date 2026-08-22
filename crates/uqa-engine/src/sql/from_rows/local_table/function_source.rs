//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for table-function sources.

use super::{
    apply_table_function_aliases, attach_qualifier_filter, build_table_function_row_stream,
    multi_unnest_internal_columns, qualify_source_operator_with_columns,
    table_function_column_types, table_function_empty_schema, validate_table_function_alias_count,
    ColumnPrune, CteScope, Engine, QualifierFilters, SQLError, SQLParam, ScopedEngineHook,
    SourceEvalContext, SourcePlan, TableFunctionCall, TableFunctionTypeRequest,
    TABLE_FUNCTION_ORDINALITY_COLUMN,
};
use uqa_execution::PhysicalOperator;

/// Build the physical operator for a table-function source.
pub(super) fn build_function_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Function {
            name,
            output_name,
            relation,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => {
            let bound_columns = crate::sql::select::user_function_output_columns(engine, name)
                .map_or_else(
                    || {
                        table_function_empty_schema(
                            name,
                            output_name,
                            alias.as_deref(),
                            column_aliases,
                            args.len(),
                            *ordinality,
                        )
                    },
                    |columns| apply_table_function_aliases(columns, column_aliases, *ordinality),
                );
            validate_table_function_alias_count(
                alias.as_deref().unwrap_or(output_name),
                bound_columns.len(),
                column_aliases.len(),
            )?;
            let hook = ScopedEngineHook::new(engine, ctes);
            let context =
                SourceEvalContext::new(engine, params, &hook, &hook, &ctes.scalar_subqueries);
            let call = TableFunctionCall {
                name,
                output_name,
                relation: relation.as_deref(),
                args,
                alias: alias.as_deref(),
                column_aliases,
                ordinality: *ordinality,
                column_types,
            };
            let rows = build_table_function_row_stream(&context, call)?;
            let multi_unnest =
                crate::sql::builtin_function_dispatch_name(name) == "unnest" && args.len() > 1;
            let (operator, source_columns): (
                Box<dyn uqa_execution::PhysicalOperator + 'a>,
                Vec<String>,
            ) = if multi_unnest {
                let public_columns = bound_columns.clone();
                let mut internal_columns = multi_unnest_internal_columns(args.len());
                if *ordinality {
                    internal_columns.push(TABLE_FUNCTION_ORDINALITY_COLUMN.into());
                }
                let types = table_function_column_types(
                    engine,
                    TableFunctionTypeRequest {
                        name,
                        args,
                        declared_types: column_types,
                        columns: &public_columns,
                        ordinality: *ordinality,
                    },
                    &uqa_execution::RowSchema::default(),
                    params,
                );
                let identities = public_columns
                    .into_iter()
                    .map(uqa_execution::ColumnIdentity::unqualified)
                    .collect();
                let schema = uqa_execution::RowSchema::with_identities(
                    internal_columns.clone(),
                    identities,
                    types,
                );
                (
                    Box::new(uqa_execution::RowIteratorScan::with_row_schema(
                        schema,
                        Box::new(rows),
                    )),
                    internal_columns,
                )
            } else {
                let mut rows = rows;
                let first = rows
                    .next()
                    .transpose()
                    .map_err(crate::sql::select::physical_exec_error)?;
                let columns = if *ordinality {
                    first.as_ref().map_or(bound_columns.clone(), |row| {
                        if row.len() == bound_columns.len() {
                            return bound_columns.clone();
                        }
                        let mut columns = row
                            .keys()
                            .filter(|column| column.as_str() != TABLE_FUNCTION_ORDINALITY_COLUMN)
                            .cloned()
                            .collect::<Vec<_>>();
                        let ordinality_column = column_aliases
                            .get(columns.len())
                            .cloned()
                            .unwrap_or_else(|| "ordinality".into());
                        columns.push(ordinality_column);
                        columns
                    })
                } else if column_aliases.is_empty() {
                    first.as_ref().map_or_else(
                        || {
                            table_function_empty_schema(
                                name,
                                output_name,
                                alias.as_deref(),
                                column_aliases,
                                args.len(),
                                false,
                            )
                        },
                        |row| row.keys().cloned().collect(),
                    )
                } else {
                    table_function_empty_schema(
                        name,
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        args.len(),
                        false,
                    )
                };
                let rows = first.into_iter().map(Ok).chain(rows);
                let types = table_function_column_types(
                    engine,
                    TableFunctionTypeRequest {
                        name,
                        args,
                        declared_types: column_types,
                        columns: &columns,
                        ordinality: *ordinality,
                    },
                    &uqa_execution::RowSchema::default(),
                    params,
                );
                if *ordinality {
                    let mut internal_columns = columns.clone();
                    if let Some(column) = internal_columns.last_mut() {
                        *column = TABLE_FUNCTION_ORDINALITY_COLUMN.into();
                    }
                    let identities = columns
                        .into_iter()
                        .map(uqa_execution::ColumnIdentity::unqualified)
                        .collect();
                    let schema = uqa_execution::RowSchema::with_identities(
                        internal_columns.clone(),
                        identities,
                        types,
                    );
                    (
                        Box::new(uqa_execution::RowIteratorScan::with_row_schema(
                            schema,
                            Box::new(rows),
                        )),
                        internal_columns,
                    )
                } else {
                    (
                        Box::new(uqa_execution::RowIteratorScan::with_types(
                            columns.clone(),
                            types,
                            Box::new(rows),
                        )),
                        columns,
                    )
                }
            };
            let qualifier = alias.as_deref().unwrap_or(output_name);
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
        _ => unreachable!("table-function source builder called for a different source kind"),
    }
}
