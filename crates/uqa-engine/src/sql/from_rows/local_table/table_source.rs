//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for tables, CTEs, views, catalogs, and foreign tables.

use super::{
    alias_query_output_to_shared, apply_propagated_view_lock, attach_qualifier_filter,
    build_info_schema_rows, combine_filters, execute_query_plan_output,
    execute_view_plan_output_with_parent_cache, materialize_plan_ctes,
    push_output_filter_into_query_plan, qualifier_filter, qualifier_for, qualify_source_operator,
    qualify_source_operator_with_columns, query_contains_volatile_function, query_cte_names,
    query_output_shared, table_lock_origin, try_build_streaming_subquery_operator,
    try_streaming_local_table_scan, virtual_relation_schema, ColumnPrune, CteScope, Engine,
    QualifierFilters, QueryOutputMode, SQLError, SQLParam, ScoredDocumentSource, ScoredInput,
    SourcePlan,
};
use uqa_execution::PhysicalOperator;

/// Build the physical operator for a table source.
pub(super) fn build_table_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            let qualifier = qualifier_for(qualifier, alias.as_deref());
            if let Some(materialized) = ctes.rows.get(name).cloned() {
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                if let Some(visible) = ctes.recursive_control_width(name) {
                    let operator: Box<dyn PhysicalOperator + 'a> =
                        Box::new(uqa_execution::ColumnSelection::hiding_trailing_columns(
                            scan, visible, &qualifier,
                        ));
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                let operator =
                    qualify_source_operator(scan, &qualifier, prune, ctes.lock_identities.emit);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(plan) = ctes.deferred_for_scan(name) {
                let streamed = {
                    let mut scoped_ctes = ctes.enter_lock_identity_emission(false);
                    try_build_streaming_subquery_operator(
                        engine,
                        &plan.query,
                        params,
                        &mut scoped_ctes,
                    )?
                };
                if let Some(operator) = streamed {
                    let source_columns = operator.schema().to_vec();
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &plan.columns,
                        false,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                let materialized =
                    if plan.materialization == uqa_sql::ast::CteMaterialization::NotMaterialized {
                        let output = {
                            let mut scoped_ctes = ctes.enter_lock_identity_emission(false);
                            execute_query_plan_output(
                                engine,
                                &plan.query,
                                params,
                                &mut scoped_ctes,
                                QueryOutputMode::SharedSpill,
                            )?
                        };
                        alias_query_output_to_shared(engine, output, &plan.columns)?
                    } else {
                        materialize_plan_ctes(engine, std::slice::from_ref(&plan), params, ctes)?;
                        ctes.rows.get(name).cloned().ok_or_else(|| {
                            SQLError::Internal(format!(
                                "deferred CTE `{name}` did not produce a materialized input"
                            ))
                        })?
                    };
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                let operator = qualify_source_operator(scan, &qualifier, prune, false);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(view) = engine.view_definition(name)? {
                if view.kind == crate::StoredViewKind::Materialized {
                    if !view.populated {
                        return Err(SQLError::Routine {
                            sqlstate: "55000".into(),
                            message: format!("materialized view \"{name}\" has not been populated"),
                        });
                    }
                    let columns = view.output_columns.unwrap_or_default();
                    let types = view.materialized_column_types;
                    let rows = view.materialized_rows;
                    let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                        uqa_execution::TableScan::from_typed_rows(columns.clone(), types, rows),
                    );
                    let operator = qualify_source_operator_with_columns(
                        scan,
                        &columns,
                        &qualifier,
                        prune,
                        &[],
                        false,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                let plan = &view.query;
                let output_columns = view.output_columns.as_deref().unwrap_or(&[]);
                let inherited_lock = ctes.source_row_lock_for_view(&qualifier, name);
                // During a tuple-local recheck, a view named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
                let mut recheck_scope = ctes.enter_recheck_storage_pins(&qualifier);
                let ctes: &mut CteScope = &mut recheck_scope;
                let specialized_plan = filters
                    .and_then(|filters| filters.get(&qualifier))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| combine_filters(filters.iter().cloned()))
                    .map(|filter| {
                        push_output_filter_into_query_plan(
                            engine,
                            plan,
                            &qualifier,
                            &filter,
                            (!output_columns.is_empty()).then_some(output_columns),
                        )
                    })
                    .transpose()?
                    .flatten();
                let propagated_plan = inherited_lock.as_ref().map(|target| {
                    let mut plan = specialized_plan.clone().unwrap_or_else(|| plan.clone());
                    apply_propagated_view_lock(&mut plan, target);
                    plan
                });
                let execution_plan = propagated_plan
                    .as_ref()
                    .or(specialized_plan.as_ref())
                    .unwrap_or(plan);
                if let Some(operator) =
                    try_build_streaming_subquery_operator(engine, execution_plan, params, ctes)?
                {
                    let source_columns = operator.schema().to_vec();
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        output_columns,
                        ctes.lock_identities.emit,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = query_contains_volatile_function(engine, execution_plan)?;
                let output = if is_volatile || propagated_plan.is_some() {
                    let mut scoped = ctes.clone();
                    execute_query_plan_output(
                        engine,
                        execution_plan,
                        params,
                        &mut scoped,
                        QueryOutputMode::SharedSpill,
                    )?
                } else {
                    execute_view_plan_output_with_parent_cache(
                        engine,
                        execution_plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let columns = output.internal_columns.clone();
                let shared = query_output_shared(output, "view")?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(shared));
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    output_columns,
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(rows) = build_info_schema_rows(engine, name)? {
                let schema = virtual_relation_schema(engine, name)?.ok_or_else(|| {
                    SQLError::Internal(format!(
                        "virtual relation `{name}` has rows but no PostgreSQL 18 row type"
                    ))
                })?;
                let (columns, types): (Vec<_>, Vec<_>) = schema
                    .into_iter()
                    .map(|(column, ty)| (column, Some(ty)))
                    .unzip();
                let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                    uqa_execution::TableScan::from_typed_rows(columns.clone(), types, rows),
                );
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &[],
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if engine
                .foreign_table(name)
                .map_err(SQLError::Unsupported)?
                .is_some()
            {
                let rows = engine
                    .scan_foreign_table_stream(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                let typed_columns = engine
                    .foreign_table_typed_columns(name)
                    .map_err(SQLError::Unsupported)?;
                let columns = typed_columns
                    .iter()
                    .map(|(column, _)| column.clone())
                    .collect::<Vec<_>>();
                let types = typed_columns.into_iter().map(|(_, ty)| Some(ty)).collect();
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::RowIteratorScan::with_types(
                        columns.clone(),
                        types,
                        Box::new(rows.map(|row| {
                            row.map_err(SQLError::Unsupported)
                                .map_err(uqa_execution::ExecError::from)
                        })),
                    ));
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &[],
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(predicate) = qualifier_filter(filters, &qualifier)
                .filter(uqa_planner::optimizer::contains_retrieval)
            {
                let lock_origin =
                    table_lock_origin(engine, name, &qualifier, ctes.lock_identities.emit)?;
                let recheck_pins =
                    lock_origin
                        .as_ref()
                        .and_then(|(origin_qualifier, storage_name)| {
                            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
                        });
                // A tuple-local recheck judges the substituted committed images with the retrieval predicate re-executed against the latest committed index state, not this session's pinned snapshot.
                let entries = if recheck_pins.is_some() {
                    engine.committed_retrieval_entries(name, &predicate, params)?
                } else {
                    crate::operator_tree_bridge::run_optimised(
                        engine,
                        name,
                        Some(&predicate),
                        params,
                    )?
                }
                .ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "JOIN filter retrieval predicate for `{qualifier}` cannot be represented by the shared operator IR"
                    ))
                })?;
                let table = engine.require_table(name)?;
                let columns = engine.try_table_columns(name).map_err(|error| {
                    SQLError::Internal(format!("read table columns for `{name}`: {error}"))
                })?;
                let source = ScoredDocumentSource::new(
                    name,
                    table,
                    ScoredInput::entries(entries, true),
                    columns,
                    None,
                    None,
                )
                .with_lock_origin(lock_origin)
                .with_recheck_pins(recheck_pins);
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::new(Box::new(source)));
                return Ok(qualify_source_operator(
                    scan,
                    &qualifier,
                    prune,
                    ctes.lock_identities.emit,
                ));
            }

            let Some((operator, filter_pushed)) =
                try_streaming_local_table_scan(engine, from, ctes, prune, filters, params)?
            else {
                return Err(SQLError::UnknownTable(name.clone()));
            };
            if filter_pushed {
                Ok(operator)
            } else {
                Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ))
            }
        }
        _ => unreachable!("table source builder called for a different source kind"),
    }
}
