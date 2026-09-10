//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for tables, CTEs, views, catalogs, and foreign tables.

use super::{
    apply_propagated_view_lock, attach_qualifier_filter, combine_filters,
    execute_view_plan_output_with_parent_cache, qualifier_filter, qualifier_for,
    qualify_source_operator_with_columns, query_cte_names, query_output_shared,
    try_build_streaming_subquery_operator, ColumnPrune, CteScope, PhysicalOperator,
    QualifierFilters, SQLError, SQLParam, SourceContext, SourcePlan, Value,
};

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
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            ..
        } => {
            let qualifier = qualifier_for(qualifier, alias.as_deref());
            if let Some(materialized) = ctes.materialized_for_scan(name) {
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::SharedSpillScan::new(materialized));
                if let Some(visible) = uqa_sql::semantics::cte_reference_name(name)
                    .and_then(|name| ctes.recursive_control_width(&name))
                {
                    let operator: Box<dyn PhysicalOperator + 'a> = Box::new(
                        crate::ColumnSelection::hiding_trailing_columns(scan, visible, &qualifier),
                    );
                    let source_columns = operator.schema().to_vec();
                    let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &aliases,
                        ctes.lock_identities.emit,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, context, params, ctes,
                    ));
                }
                let source_columns = scan.schema().to_vec();
                let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &source_columns,
                    &qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }

            if let Some(plan) = ctes.deferred_for_scan(name) {
                let streamed = {
                    let mut scoped_ctes = ctes.enter_lock_identity_emission(false);
                    match plan.body.query() {
                        Some(query) => try_build_streaming_subquery_operator(
                            context,
                            query,
                            params,
                            &mut scoped_ctes,
                        )?,
                        None => None,
                    }
                };
                if let Some(operator) = streamed {
                    let source_columns = operator.schema().to_vec();
                    let aliases =
                        table_source_aliases(&source_columns, &plan.columns, column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &aliases,
                        false,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, context, params, ctes,
                    ));
                }
                let materialized = if plan.materialization
                    == uqa_sql::ast::CteMaterialization::NotMaterialized
                    && !plan.body.modifies_data()
                {
                    let output = {
                        let mut scoped_ctes = ctes.enter_lock_identity_emission(false);
                        context.ctes.queries.execute_query(
                            plan.body.query().ok_or_else(|| {
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
                    )?
                } else {
                    crate::query::cte::materialize_plan_ctes(
                        context.ctes,
                        std::slice::from_ref(&plan),
                        params,
                        ctes,
                    )?;
                    ctes.materialized_for_scan(name).ok_or_else(|| {
                        SQLError::Internal(format!(
                            "deferred CTE `{name}` did not produce a materialized input"
                        ))
                    })?
                };
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::SharedSpillScan::new(materialized));
                let source_columns = scan.schema().to_vec();
                let aliases = table_source_aliases(&source_columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &source_columns,
                    &qualifier,
                    prune,
                    &aliases,
                    false,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }

            let catalog = ctes.catalog_read_view()?;
            let resolution = ctes.relation_name_resolution()?;
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
                    scan, &columns, &qualifier, prune, &aliases, false,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }
            if let Some(view) = catalog.view_resolved(&resolution, name)?.cloned() {
                if view.kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
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
                        crate::TableScan::from_typed_rows(columns.clone(), types, rows),
                    );
                    let aliases = table_source_aliases(&columns, &[], column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        scan, &columns, &qualifier, prune, &aliases, false,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, context, params, ctes,
                    ));
                }
                let privilege_subject = if view.security_invoker() {
                    ctes.privilege_subject()?.to_string()
                } else {
                    view.role_owner.clone()
                };
                let mut privilege_scope = ctes.enter_privilege_subject(privilege_subject);
                let ctes: &mut CteScope<S> = &mut privilege_scope;
                let plan = &view.query;
                let output_columns = view.output_columns.as_deref().unwrap_or(&[]);
                let inherited_lock = ctes.source_row_lock_for_view(&qualifier, name);
                // During a tuple-local recheck, a view named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
                let mut recheck_scope = ctes.enter_recheck_storage_pins(&qualifier);
                let ctes: &mut CteScope<S> = &mut recheck_scope;
                let specialized_plan = column_aliases
                    .is_empty()
                    .then_some(filters)
                    .flatten()
                    .and_then(|filters| filters.get(&qualifier))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| combine_filters(filters.iter().cloned()))
                    .map(|filter| {
                        context.ctes.rewrites.push_output_filter(
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
                    try_build_streaming_subquery_operator(context, execution_plan, params, ctes)?
                {
                    let source_columns = operator.schema().to_vec();
                    let aliases =
                        table_source_aliases(&source_columns, output_columns, column_aliases);
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &aliases,
                        ctes.lock_identities.emit,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, context, params, ctes,
                    ));
                }
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = uqa_sql::semantics::volatility::query_contains_volatile_function(
                    context.volatility,
                    execution_plan,
                )?;
                let output = if is_volatile || propagated_plan.is_some() {
                    let mut scoped = ctes.clone();
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
                let columns = output.internal_columns.clone();
                let shared = query_output_shared(output, "view")?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::SharedSpillScan::new(shared));
                let aliases = table_source_aliases(&columns, output_columns, column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }

            if let Some(rows) = crate::catalog::projection::build_info_schema_rows(
                &context.catalog,
                &catalog,
                &resolution,
                context.catalog.session,
                name,
            )? {
                let schema =
                    crate::catalog::schema::virtual_relation_schema(&catalog, &resolution, name)?
                        .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "virtual relation `{name}` has rows but no PostgreSQL 18 row type"
                        ))
                    })?;
                let (columns, types): (Vec<_>, Vec<_>) = schema
                    .into_iter()
                    .map(|(column, ty)| (column, Some(ty)))
                    .unzip();
                let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                    crate::TableScan::from_typed_rows(columns.clone(), types, rows),
                );
                let aliases = table_source_aliases(&columns, &[], column_aliases);
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }

            if let Some((foreign_name, foreign_table)) =
                catalog.foreign_table_entry_resolved(&resolution, name)?
            {
                let rows = context
                    .foreign_tables
                    .scan_foreign_source(&foreign_name, &[])
                    .map_err(SQLError::Unsupported)?;
                let typed_columns = foreign_table
                    .columns
                    .into_iter()
                    .map(|column| (column.name, column.ty))
                    .collect::<Vec<_>>();
                let columns = typed_columns
                    .iter()
                    .map(|(column, _)| column.clone())
                    .collect::<Vec<_>>();
                let types = typed_columns.into_iter().map(|(_, ty)| Some(ty)).collect();
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(crate::RowIteratorScan::with_types(
                        columns.clone(),
                        types,
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
                    &qualifier,
                    prune,
                    &aliases,
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ));
            }

            if let Some(predicate) =
                qualifier_filter(filters, &qualifier).filter(uqa_sql::semantics::contains_retrieval)
            {
                return crate::query::table_sources::hierarchy::build_hierarchy_retrieval_operator(
                    context.retrieval,
                    from,
                    &qualifier,
                    &predicate,
                    params,
                    ctes,
                    prune,
                );
            }

            let Some((operator, filter_pushed)) =
                crate::query::table_sources::scan::try_streaming_local_table_scan(
                    context.scans,
                    from,
                    ctes,
                    prune,
                    filters,
                    params,
                )?
            else {
                return Err(SQLError::UnknownTable(name.clone()));
            };
            if filter_pushed {
                Ok(operator)
            } else {
                Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, context, params, ctes,
                ))
            }
        }
        _ => unreachable!("table source builder called for a different source kind"),
    }
}
