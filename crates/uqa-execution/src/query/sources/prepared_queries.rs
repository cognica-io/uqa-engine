//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare named relational inputs once, before any statement input produces rows.

use super::{ColumnPrune, CteScope, RowSchema, SourceContext};
use std::{borrow::Cow, collections::BTreeSet};
use uqa_sql::{
    binding::SourceScope,
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    SQLError, SQLParam,
};

pub(crate) fn prepare<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &'a QueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<Cow<'a, QueryPlan>, SQLError> {
    let mut scope = scope.clone();
    for cte in &query.ctes {
        scope.insert_deferred(cte.clone());
    }
    if !needs_preparation(context, query, &scope)? {
        return Ok(Cow::Borrowed(query));
    }
    let mut query = query.clone();
    prepare_query(
        context,
        &mut query,
        params,
        &scope,
        &BTreeSet::new(),
        scope.row_lock_outer_row().map(|row| &row.schema),
    )?;
    Ok(Cow::Owned(query))
}

fn needs_preparation<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &QueryPlan,
    scope: &CteScope<S>,
) -> Result<bool, SQLError> {
    fn source<S: Clone + Send + Sync + 'static>(
        context: &SourceContext<'_, S>,
        from: &SourcePlan,
        scope: &CteScope<S>,
    ) -> Result<bool, SQLError> {
        Ok(match from {
            SourcePlan::Table {
                name,
                executable_query: None,
                ..
            } => {
                if scope.is_visible_cte(name) {
                    scope.deferred_reference(name).is_some()
                } else {
                    scope
                        .catalog_read_view()?
                        .view_resolved(&scope.relation_name_resolution()?, name)?
                        .is_some_and(|view| {
                            view.kind == uqa_sql::catalog::view::StoredViewKind::View
                        })
                }
            }
            SourcePlan::Join { left, right, .. } => {
                source(context, left, scope)? || source(context, right, scope)?
            }
            SourcePlan::Subquery { body, .. } => needs_preparation(context, body, scope)?,
            _ => false,
        })
    }
    let mut scope = scope.clone();
    for cte in &query.ctes {
        scope.insert_deferred(cte.clone());
    }
    let mut lookup = scope.enter_relation_lookup_mode(query.relations_bound)?;
    let scope = lookup.enter_visible_ctes(query.ctes.iter().map(|cte| cte.name.as_str()));
    let scope = &*scope;
    for cte in &query.ctes {
        if let Some(query) = cte.body.query() {
            if needs_preparation(context, query, scope)? {
                return Ok(true);
            }
        }
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(from) = &block.from {
                if source(context, from, scope)? {
                    return Ok(true);
                }
            }
            for query in &block.subqueries {
                if needs_preparation(context, query, scope)? {
                    return Ok(true);
                }
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            if needs_preparation(context, left, scope)? || needs_preparation(context, right, scope)?
            {
                return Ok(true);
            }
            for query in subqueries {
                if needs_preparation(context, query, scope)? {
                    return Ok(true);
                }
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                if needs_preparation(context, query, scope)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn prepare_query<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &mut QueryPlan,
    params: &[SQLParam],
    parent: &CteScope<S>,
    parent_folded: &BTreeSet<String>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut scope = parent.clone();
    let mut folded = parent_folded.clone();
    for cte in &query.ctes {
        folded.remove(&cte.name);
        scope.insert_deferred(cte.clone());
    }
    let mut materialized = BTreeSet::new();
    for scheduled in crate::query::cte::strategy::schedule_plan_ctes(context.volatility, query)? {
        if scheduled.deferred {
            folded.insert(scheduled.plan.name.clone());
        } else {
            materialized.insert(scheduled.plan.name.clone());
        }
    }
    let mut lookup = scope.enter_relation_lookup_mode(query.relations_bound)?;
    let names = query
        .ctes
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<Vec<_>>();
    let mut scope = lookup.enter_visible_ctes(names.iter().map(String::as_str));
    for cte in &mut query.ctes {
        // Folded inputs are prepared at each selected reference; unused inputs never execute.
        if !materialized.contains(&cte.name) {
            continue;
        }
        if let Some(body) = cte.body.query_mut() {
            prepare_query(context, body, params, &scope, &folded, outer)?;
        }
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            scope.scalar_subqueries = block.subqueries.as_slice().into();
            let mut source_schema = RowSchema::default();
            if let Some(from) = &block.from {
                source_schema = crate::query::binding::bind_source_plan_schema(
                    context.ctes.routines,
                    from,
                    params,
                    &scope,
                    outer,
                )?;
                let prune =
                    context
                        .planning
                        .column_prune(block, from, &scope, Some(&source_schema))?;
                prepare_source(
                    context,
                    block.from.as_mut().expect("source checked"),
                    params,
                    &scope,
                    &folded,
                    prune.as_ref(),
                    SourceScope::new(outer),
                )?;
            }
            let expression_schema = uqa_sql::binding::overlay_outer_schema(&source_schema, outer);
            for query in &mut block.subqueries {
                prepare_query(
                    context,
                    query,
                    params,
                    &scope,
                    &folded,
                    Some(&expression_schema),
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            prepare_query(context, left, params, &scope, &folded, outer)?;
            prepare_query(context, right, params, &scope, &folded, outer)?;
            for query in subqueries {
                prepare_query(context, query, params, &scope, &folded, outer)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                prepare_query(context, query, params, &scope, &folded, outer)?;
            }
        }
    }
    Ok(())
}

fn prepare_source<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    source: &mut SourcePlan,
    params: &[SQLParam],
    parent: &CteScope<S>,
    folded: &BTreeSet<String>,
    prune: Option<&ColumnPrune>,
    names: SourceScope<'_>,
) -> Result<(), SQLError> {
    let input = (!matches!(source, SourcePlan::Join { .. }))
        .then(|| names.input(source))
        .flatten();
    let outer = input.as_ref();
    match source {
        SourcePlan::Table { .. } => {
            prepare_named_source(context, source, params, parent, folded, prune, outer)?;
        }
        SourcePlan::Join {
            left,
            right,
            lateral,
            kind,
            ..
        } => {
            prepare_source(
                context,
                left,
                params,
                parent,
                folded,
                prune,
                names.with_names(),
            )?;
            let left_schema = uqa_sql::binding::bind_source_plan_schema_in_scope(
                context.ctes.routines,
                left,
                params,
                &crate::query::binding::binding_context(parent)?,
                names.with_names(),
            )?;
            let preceding = names.right_namespace(left, &left_schema, *kind);
            prepare_source(
                context,
                right,
                params,
                parent,
                folded,
                prune,
                SourceScope {
                    outer: names.outer,
                    preceding: Some(&preceding),
                    inherited_lateral: *lateral,
                    retain_names: true,
                },
            )?;
        }
        SourcePlan::Subquery { body, .. } => {
            prepare_query(context, body, params, parent, folded, outer)?;
        }
        _ => {}
    }
    Ok(())
}

fn prepare_named_source<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    source: &mut SourcePlan,
    params: &[SQLParam],
    parent: &CteScope<S>,
    folded: &BTreeSet<String>,
    prune: Option<&ColumnPrune>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let SourcePlan::Table {
        name,
        qualifier,
        alias,
        column_aliases,
        executable_query,
        ..
    } = source
    else {
        unreachable!("named source checked by the traversal");
    };
    if executable_query.is_some() {
        return Ok(());
    }
    let qualifier = alias.as_deref().unwrap_or(qualifier);
    let mut scope = parent.clone();
    let (mut query, aliases, optimize) = if let Some(cte) = scope.deferred_reference(name) {
        if !folded.contains(&cte.name) {
            return Ok(());
        }
        let Some(query) = cte.body.query() else {
            return Ok(());
        };
        (
            query.clone(),
            super::table_source::output_aliases(&cte.columns, column_aliases),
            false,
        )
    } else {
        if scope.is_visible_cte(name) {
            return Ok(());
        }
        let catalog = scope.catalog_read_view()?;
        let Some(view) = catalog
            .view_resolved(&scope.relation_name_resolution()?, name)?
            .cloned()
        else {
            return Ok(());
        };
        if view.kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            return Ok(());
        }
        let subject = if view.security_invoker() {
            scope.privilege_subject()?.clone()
        } else {
            catalog.view_owner(&view)?
        };
        scope = {
            let scoped = scope.enter_privilege_subject(subject);
            (*scoped).clone()
        };
        (
            view.query.clone(),
            super::table_source::output_aliases(
                view.output_columns.as_deref().unwrap_or_default(),
                column_aliases,
            ),
            true,
        )
    };
    let _stored = optimize
        .then(uqa_sql::semantics::sets::placement_scope::SetPlacementScope::stored_definition);
    if let Some(projection) = prune.and_then(|prune| prune.get(qualifier)) {
        super::table_source::prune_query_outputs(
            context, &mut query, &aliases, projection, params, &scope, outer,
        )?;
    }
    if optimize {
        query = context.ctes.rewrites.optimize_retained_query(&query)?;
    }
    prepare_query(context, &mut query, params, &scope, folded, outer)?;
    *executable_query = Some(Box::new(query));
    Ok(())
}
