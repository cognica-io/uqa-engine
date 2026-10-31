//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for tables, CTEs, views, catalogs, and foreign tables.

use super::{
    attach_qualifier_filter, execute_view_plan_output_with_parent_cache,
    qualify_source_operator_with_columns, query_cte_names_with_control, query_output_shared,
    try_build_streaming_subquery_operator, ColumnPrune, CteScope, PhysicalOperator,
    QualifierFilters, SQLError, SQLParam, SourceContext, SourcePlan, Value,
};
use uqa_sql::semantics::sets::placement_scope::SetPlacementScope;

pub(super) mod production;

fn table_source_aliases(
    source_columns: &[String],
    catalog_aliases: &[String],
    range_aliases: &[String],
) -> Vec<String> {
    let mut aliases = source_columns.to_vec();
    for (column, alias) in aliases.iter_mut().zip(catalog_aliases) {
        column.clone_from(alias);
    }
    for (column, alias) in aliases.iter_mut().zip(range_aliases) {
        column.clone_from(alias);
    }
    aliases
}

/// Build the physical operator for a table source.
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn build_table_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Table {
            name,
            executable_query,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            ..
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            if let Some(materialized) = ctes.materialized_for_scan(name) {
                let scan: Box<dyn PhysicalOperator + 'a> = materialized.into_operator();
                if let Some(visible) = uqa_sql::semantics::cte_reference_name(name)
                    .and_then(|name| ctes.recursive_control_width(&name))
                {
                    let operator: Box<dyn PhysicalOperator + 'a> = Box::new(
                        crate::ColumnSelection::hiding_trailing_columns(scan, visible, qualifier),
                    );
                    let source_columns = operator.schema().to_vec();
                    let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        qualifier,
                        prune,
                        &aliases,
                        ctes.lock_identities.emit,
                    );
                    return attach_qualifier_filter(
                        operator, qualifier, filters, context, params, ctes,
                    );
                }
                let source_columns = scan.schema().to_vec();
                let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &source_columns,
                    qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }

            if let Some((plan, mut definition_scope)) = ctes.deferred_input_with_control(
                name,
                &context.ctes.production_resources()?.control(),
            )? {
                let resources = context.ctes.production_resources()?;
                let control = resources.control();
                control.check()?;
                let selected = plan
                    .body
                    .query()
                    .map(|query| executable_query.as_deref().unwrap_or(query));
                let mut rewritten = None;
                if let (Some(query), Some(projection)) = (
                    selected.filter(|_| executable_query.is_none()),
                    prune.and_then(|prune| prune.get(qualifier)),
                ) {
                    let aliases =
                        production::output_aliases(&plan.columns, column_aliases, &control)?;
                    let query = production::prune_outputs(
                        context,
                        query.copy_for_rewrite(&control)?,
                        &aliases,
                        projection,
                        production::QueryBinding {
                            params,
                            scope: &definition_scope,
                            outer: definition_scope.row_lock_outer_row().map(|row| &row.schema),
                        },
                        &control,
                    )?;
                    rewritten = Some(uqa_sql::plan::OwnedQueryPlan::from_produced(
                        query.finish(&control)?,
                        &control,
                    )?);
                }
                let specialized = match rewritten.as_deref().or(selected) {
                    Some(query) => {
                        let aliases =
                            production::output_aliases(&plan.columns, column_aliases, &control)?;
                        super::output_filter::specialize_source_filter(
                            context, query, qualifier, &aliases, filters,
                        )?
                    }
                    None => None,
                };
                let filters = if let Some(specialized) = specialized {
                    rewritten = Some(specialized);
                    None
                } else {
                    filters
                };
                let query = rewritten.as_deref().or(selected);
                let streamed = {
                    let mut scoped_ctes = definition_scope.enter_lock_identity_emission(false);
                    match query {
                        Some(query)
                            if scoped_ctes.streams_command_progress()
                                || !uqa_sql::semantics::volatility::query_contains_volatile_function(
                                    context.volatility,
                                    query,
                                )? =>
                        {
                            match try_build_streaming_subquery_operator(context, query, params, &mut scoped_ctes)? {
                                Some(operator) => Some(operator),
                                None => context.ctes.queries.retained_query(query, params, &scoped_ctes)?,
                            }
                        }
                        _ => None,
                    }
                };
                if let Some(operator) = streamed {
                    let source_columns = operator.schema().to_vec();
                    let aliases =
                        table_source_aliases(&source_columns, &plan.columns, column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        qualifier,
                        prune,
                        &aliases,
                        false,
                    );
                    return attach_qualifier_filter(
                        operator, qualifier, filters, context, params, ctes,
                    );
                }
                let materialized = if plan.materialization
                    == uqa_sql::ast::CteMaterialization::NotMaterialized
                    && !plan.body.modifies_data()
                {
                    let output = {
                        let mut scoped_ctes = definition_scope.enter_lock_identity_emission(false);
                        context.ctes.queries.execute_query(
                            query.ok_or_else(|| {
                                SQLError::Internal(
                                    "data-modifying CTE entered the folded query path".into(),
                                )
                            })?,
                            params,
                            &mut scoped_ctes,
                        )?
                    };
                    crate::query::cte::recursive::alias_query_output_to_shared(
                        context.ctes,
                        output,
                        &plan.columns,
                    )
                    .map(crate::query::cte::rows::CteRows::Complete)?
                } else {
                    let replacement = if rewritten.is_some()
                        || (selected.is_some() && executable_query.is_some())
                    {
                        let body = match rewritten.take() {
                            Some(query) => query,
                            None => uqa_sql::plan::OwnedQueryPlan::from_produced(
                                selected
                                    .expect("selected CTE query")
                                    .clone_with_control(&control)?,
                                &control,
                            )?,
                        };
                        Some(plan.copy_with_query(body, &control)?)
                    } else {
                        None
                    };
                    crate::query::cte::materialize_plan_ctes(
                        context.ctes,
                        std::slice::from_ref(replacement.as_deref().unwrap_or(&plan)),
                        params,
                        &mut definition_scope,
                    )?;
                    let rows = definition_scope
                        .materialized_for_scan(name)
                        .ok_or_else(|| {
                            SQLError::Internal(format!(
                                "deferred CTE `{name}` did not produce a materialized input"
                            ))
                        })?;
                    ctes.insert_rows(plan.name.clone(), rows.clone());
                    rows
                };
                let scan: Box<dyn PhysicalOperator + 'a> = materialized.into_operator();
                let source_columns = scan.schema().to_vec();
                let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &source_columns,
                    qualifier,
                    prune,
                    &aliases,
                    false,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }

            let catalog = ctes.catalog_read_view()?;
            let resources = context.ctes.production_resources()?;
            let control = resources.control();
            let resolution = ctes.relation_name_resolution_with_control(&control)?;
            if let Some(sequence) = catalog.sequence_resolved(&resolution, name)? {
                let privilege_subject = ctes.privilege_subject()?;
                if !catalog.sequence_is_selectable_to(&sequence.security, privilege_subject) {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!(
                            "permission denied for sequence {}",
                            sequence.relation.name
                        ),
                    });
                }
                let columns = vec!["last_value".into(), "log_cnt".into(), "is_called".into()];
                let types = vec![
                    Some(uqa_sql::ast::ColumnType::BigInteger),
                    Some(uqa_sql::ast::ColumnType::BigInteger),
                    Some(uqa_sql::ast::ColumnType::Boolean),
                ];
                let rows = vec![std::collections::BTreeMap::from([
                    ("last_value".into(), Value::Int(sequence.state.current)),
                    ("log_cnt".into(), Value::Int(sequence.state.log_count)),
                    ("is_called".into(), Value::Bool(sequence.state.called)),
                ])];
                let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                    crate::TableScan::from_typed_rows(columns.clone(), types, rows),
                );
                let aliases = table_source_aliases(&columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan, &columns, qualifier, prune, &aliases, false,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }
            if let Some(view) = catalog.view_resolved_with_control(&resolution, name, &control)? {
                if view.kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
                    uqa_sql::catalog::view::require_materialized_view_population(
                        name,
                        view.populated,
                    )?;
                    let schema = view.definition.materialized_schema()?;
                    let columns = schema.columns().to_vec();
                    let rows = view.definition.materialized_rows.clone();
                    let scan: Box<dyn PhysicalOperator + 'a> =
                        Box::new(crate::TableScan::from_rows_with_schema(schema, rows));
                    let aliases = table_source_aliases(&columns, &[], column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        scan, &columns, qualifier, prune, &aliases, false,
                    );
                    return attach_qualifier_filter(
                        operator, qualifier, filters, context, params, ctes,
                    );
                }
                let privilege_subject = if view.security_invoker() {
                    ctes.invoking_role()?.clone_with_control(&control)?
                } else {
                    control.finish(
                        catalog.view_owner_with_control(view.security.role_owner, &control)?,
                        control.empty_reservation(),
                    )?
                };
                let mut privilege_scope =
                    ctes.enter_privilege_subject_with_control(privilege_subject, &control)?;
                let ctes: &mut CteScope<S> = &mut privilege_scope;
                // A view runs its stored query as it was created; its set-returning calls are not placed again.
                let _stored = SetPlacementScope::stored_definition();
                let plan = executable_query.as_deref().unwrap_or(&view.query);
                let output_columns = view.output_columns.as_deref().unwrap_or(&[]);
                let inherited_lock = ctes.source_row_lock_for_view(qualifier, name);
                // During a tuple-local recheck, a view named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
                let mut recheck_scope = ctes.enter_recheck_storage_pins(qualifier);
                let ctes: &mut CteScope<S> = &mut recheck_scope;
                let aliases = production::output_aliases(output_columns, column_aliases, &control)?;
                let specialized_plan = super::output_filter::specialize_source_filter(
                    context, plan, qualifier, &aliases, filters,
                )?;
                // Successful specialization applies this complete predicate inside the view; retrieval leaves cannot be evaluated again as scalar functions.
                let filters = if specialized_plan.is_some() {
                    None
                } else {
                    filters
                };
                let mut execution_plan = specialized_plan
                    .as_deref()
                    .unwrap_or(plan)
                    .copy_for_rewrite(&control)?;
                if let Some(target) = &inherited_lock {
                    execution_plan = uqa_sql::semantics::locking::apply_propagated_view_lock_owned(
                        execution_plan,
                        target,
                        &control,
                    )?;
                }
                if executable_query.is_none() {
                    execution_plan = production::restore_layouts(
                        context,
                        execution_plan,
                        params,
                        ctes,
                        ctes.row_lock_outer_row().map(|row| &row.schema),
                        &control,
                    )?;
                }
                if let Some(projection) = prune
                    .and_then(|prune| prune.get(qualifier))
                    .filter(|_| executable_query.is_none())
                {
                    execution_plan = production::prune_outputs(
                        context,
                        execution_plan,
                        &aliases,
                        projection,
                        production::QueryBinding {
                            params,
                            scope: ctes,
                            outer: ctes.row_lock_outer_row().map(|row| &row.schema),
                        },
                        &control,
                    )?;
                }
                let optimized = if executable_query.is_some() {
                    uqa_sql::plan::OwnedQueryPlan::from_produced(
                        execution_plan.finish(&control)?,
                        &control,
                    )?
                } else {
                    context
                        .ctes
                        .rewrites
                        .optimize_retained_query(execution_plan, &control)?
                };
                let execution_plan = optimized.as_ref();
                let streamed = match try_build_streaming_subquery_operator(
                    context,
                    execution_plan,
                    params,
                    ctes,
                )? {
                    Some(operator) => Some(operator),
                    None => context
                        .ctes
                        .queries
                        .retained_query(execution_plan, params, ctes)?,
                };
                if let Some(operator) = streamed {
                    let source_columns = operator.schema().to_vec();
                    let aliases =
                        table_source_aliases(&source_columns, output_columns, column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        qualifier,
                        prune,
                        &aliases,
                        ctes.lock_identities.emit,
                    );
                    return attach_qualifier_filter(
                        operator, qualifier, filters, context, params, ctes,
                    );
                }
                let local_cte_names = query_cte_names_with_control(
                    execution_plan,
                    &context.ctes.production_resources()?.control(),
                )?;
                let is_volatile = uqa_sql::semantics::volatility::query_contains_volatile_function(
                    context.volatility,
                    execution_plan,
                )?;
                let output = if is_volatile || inherited_lock.is_some() {
                    let mut scoped =
                        ctes.copy_with_control(&context.ctes.production_resources()?.control())?;
                    context
                        .ctes
                        .queries
                        .execute_query(execution_plan, params, &mut scoped)?
                } else {
                    execute_view_plan_output_with_parent_cache(
                        context,
                        execution_plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let columns = output.internal_schema.columns().to_vec();
                let shared = query_output_shared(output, "view")?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::SharedSpillScan::new(shared));
                let aliases = table_source_aliases(&columns, output_columns, column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }

            if let Some(schema) =
                crate::catalog::schema::virtual_relation_schema(&catalog, &resolution, name)?
            {
                let (columns, types): (Vec<_>, Vec<_>) = schema
                    .into_iter()
                    .map(|(column, ty)| (column, Some(ty)))
                    .unzip();
                let schema = crate::RowSchema::with_types(columns.clone(), types);
                let tracks_reads = catalog
                    .virtual_relation_resolved(&resolution, name)?
                    .is_none_or(|relation| {
                        uqa_sql::catalog::SystemRelation::Projected(relation)
                            .tracks_serializable_reads()
                    });
                let catalog = if tracks_reads {
                    context.catalog.catalog.bind_query_reads(catalog)?
                } else {
                    catalog
                };
                let catalog = context.catalog.catalog.retain_catalog_rows(catalog)?;
                let projection = context.catalog;
                let name = name.clone();
                let aliases = table_source_aliases(&columns, &[], column_aliases);
                let request = crate::query::source_projection::catalog_request(
                    &columns, &aliases, qualifier, prune, filters,
                );
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::query::deferred::DeferredOperator::from_rows(
                        schema.clone(),
                        Box::new(move || {
                            let rows = crate::catalog::projection::build_requested_catalog_rows(
                                &projection,
                                &catalog,
                                &resolution,
                                projection.session,
                                &name,
                                &request,
                            )?
                            .ok_or_else(|| {
                                SQLError::Internal(format!(
                                    "virtual relation `{name}` disappeared from retained catalog"
                                ))
                            })?;
                            Ok(Box::new(crate::scan::VecSource::with_row_schema(
                                schema, rows,
                            )))
                        }),
                    ));
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }

            if let Some((foreign_name, foreign_table)) =
                catalog.foreign_table_entry_resolved(&resolution, name)?
            {
                let definitions = &catalog.snapshot().definitions;
                foreign_table
                    .bound_server(&definitions.foreign_servers)?
                    .bound_wrapper(&definitions.foreign_wrappers)?
                    .require_handler()?;
                let rows = context
                    .scan_foreign_source(&foreign_name, None, &[])
                    .map_err(SQLError::Unsupported)?;
                let schema =
                    crate::RowSchema::with_column_definitions(None, &foreign_table.columns);
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::RowIteratorScan::with_row_schema(
                        schema,
                        Box::new(rows.map(|row| {
                            row.map_err(SQLError::Unsupported)
                                .map_err(crate::ExecError::from)
                        })),
                    ));
                let scan = crate::query::source_projection::bound_source_operator(
                    scan,
                    bound_columns.as_deref(),
                )?;
                let columns = scan.schema().to_vec();
                let aliases = table_source_aliases(&columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return attach_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }

            if filters
                .and_then(|filters| filters.get(qualifier))
                .is_some_and(|parts| parts.iter().any(uqa_sql::semantics::contains_retrieval))
            {
                let resources = context.ctes.production_resources()?;
                let predicate = uqa_sql::semantics::source_filters::qualifier_filter_with_control(
                    filters,
                    qualifier,
                    &resources.control(),
                )?
                .expect("selected retrieval predicate");
                return crate::query::table_sources::hierarchy::build_hierarchy_retrieval_operator(
                    context.retrieval,
                    from,
                    qualifier,
                    &predicate,
                    params,
                    ctes,
                    prune,
                );
            }

            let resources = context.ctes.production_resources()?;
            let Some((operator, filter_pushed)) =
                crate::query::table_sources::scan::try_streaming_local_table_scan_with_control(
                    context.scans,
                    from,
                    ctes,
                    prune,
                    filters,
                    params,
                    &resources.control(),
                )?
            else {
                return Err(SQLError::UnknownTable(name.clone()));
            };
            if filter_pushed {
                Ok(operator)
            } else {
                attach_qualifier_filter(operator, qualifier, filters, context, params, ctes)
            }
        }
        _ => unreachable!("table source builder called for a different source kind"),
    }
}
