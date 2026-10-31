//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare named relational inputs once, before any statement input produces rows.

use super::{ColumnPrune, CteScope, RowSchema, SourceContext};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    binding::SourceScope,
    plan::{OwnedQueryPlan, QueryPlan, RelationalPlan, SourcePlan},
    SQLError, SQLParam,
};

mod inspection;

pub(crate) enum PreparedQuery<'a> {
    Borrowed(&'a QueryPlan),
    Owned(OwnedQueryPlan),
}

impl AsRef<QueryPlan> for PreparedQuery<'_> {
    fn as_ref(&self) -> &QueryPlan {
        self
    }
}

impl std::ops::Deref for PreparedQuery<'_> {
    type Target = QueryPlan;
    fn deref(&self) -> &QueryPlan {
        match self {
            Self::Borrowed(query) => query,
            Self::Owned(query) => query,
        }
    }
}

pub(crate) fn prepare<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &'a QueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<PreparedQuery<'a>, SQLError> {
    let retained = context.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.ctes.runtime.cancellation,
    );
    if !inspection::needed(query, scope, control)? {
        return Ok(PreparedQuery::Borrowed(query));
    }
    let query = OwnedQueryPlan::from_produced(query.clone_with_control(&control)?, &control)?;
    prepare_owned(
        context,
        query,
        params,
        scope,
        scope.row_lock_outer_row().map(|row| &row.schema),
    )
    .map(PreparedQuery::Owned)
}

/// Private preparation keeps the input and every newly attached executable's leases together. It only fills source attachments and shared analysis descriptors; existing scalar and source storage is not removed.
pub(crate) fn prepare_in_place<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: OwnedQueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<OwnedQueryPlan, SQLError> {
    let retained = context.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.ctes.runtime.cancellation,
    );
    if !inspection::needed(&query, scope, control)? {
        return Ok(query);
    }
    prepare_owned(
        context,
        query,
        params,
        scope,
        scope.row_lock_outer_row().map(|row| &row.schema),
    )
}

fn prepare_owned<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: OwnedQueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<OwnedQueryPlan, SQLError> {
    struct Preparation {
        query: Box<QueryPlan>,
        memory: uqa_sql::plan::ExecutableMemory,
    }
    let (query, memory) = query.into_parts();
    let mut preparation = Preparation { query, memory };
    prepare_query(
        context,
        &mut preparation.query,
        params,
        scope,
        outer,
        &mut preparation.memory,
    )?;
    Ok(OwnedQueryPlan::from_parts(
        preparation.query,
        preparation.memory,
    ))
}

fn declare_ctes<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &mut QueryPlan,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    folded: &BTreeSet<String>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut restored_outputs = BTreeMap::new();
    let retained = context.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.ctes.runtime.cancellation,
    );
    let ordered = uqa_sql::semantics::order_cte_plans_with_control(&query.ctes, control)?;
    for cte in ordered.iter().copied() {
        let (mut definition, definition_memory) = cte.clone_with_control(&control)?.into_parts();
        if !uqa_sql::semantics::cte_references_own_name_with_control(cte, control)? {
            if let Some(body) = definition
                .body
                .query_mut()
                .filter(|body| body.analyzed_output.is_none())
            {
                // Stored syntax omits transient layouts. Describe each definition before a later declaration replaces any of its inputs.
                let schema = crate::query::binding::bind_query_plan_schema(
                    context.ctes.routines,
                    body,
                    params,
                    scope,
                    outer,
                )?;
                body.analyzed_output = Some(schema.clone());
                restored_outputs.insert(cte.name.clone(), schema);
            }
        }
        scope.insert_preparation_cte(
            uqa_sql::plan::RetainedCtePlan::from_produced(
                control.finish(definition, definition_memory)?,
                &control,
            )?,
            folded.contains(&cte.name),
        );
    }
    drop(ordered);
    for cte in &mut query.ctes {
        if let Some(schema) = restored_outputs.remove(&cte.name) {
            cte.body
                .query_mut()
                .expect("query output restored")
                .analyzed_output = Some(schema);
        }
    }
    Ok(())
}

fn prepare_query<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: &mut QueryPlan,
    params: &[SQLParam],
    parent: &CteScope<S>,
    outer: Option<&RowSchema>,
    memory: &mut uqa_sql::plan::ExecutableMemory,
) -> Result<(), SQLError> {
    let mut scope = parent.clone();
    let resources = context.ctes.production_resources()?;
    let mut lookup = scope.enter_query_plan_scope_with_control(query, &resources.control())?;
    let scheduled =
        crate::query::cte::strategy::schedule_plan_ctes(context.ctes, context.volatility, query)?;
    let folded = scheduled
        .iter()
        .filter(|cte| cte.deferred)
        .map(|cte| cte.plan.name.clone())
        .collect::<BTreeSet<_>>();
    let materialized = scheduled
        .iter()
        .filter(|cte| !cte.deferred)
        .map(|cte| cte.plan.name.clone())
        .collect::<BTreeSet<_>>();
    drop(scheduled);
    declare_ctes(context, query, params, &mut lookup, &folded, outer)?;
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
        let definition_scope = scope
            .definition_scope_for_name(&cte.name)
            .expect("declared CTE scope");
        if let Some(body) = cte.body.query_mut() {
            prepare_query(context, body, params, &definition_scope, outer, memory)?;
        }
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            let arena_resources = context.ctes.production_resources()?;
            scope.scalar_subqueries = uqa_sql::plan::RetainedQueryArena::capture(
                &block.subqueries,
                &arena_resources.control(),
            )?;
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
                    prune.as_ref(),
                    SourceScope::new(outer),
                    memory,
                )?;
            }
            let expression_schema = uqa_sql::binding::overlay_outer_schema(&source_schema, outer);
            for query in &mut block.subqueries {
                prepare_query(
                    context,
                    query,
                    params,
                    &scope,
                    Some(&expression_schema),
                    memory,
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            prepare_query(context, left, params, &scope, outer, memory)?;
            prepare_query(context, right, params, &scope, outer, memory)?;
            for query in subqueries {
                prepare_query(context, query, params, &scope, outer, memory)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                prepare_query(context, query, params, &scope, outer, memory)?;
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
    prune: Option<&ColumnPrune>,
    names: SourceScope<'_>,
    memory: &mut uqa_sql::plan::ExecutableMemory,
) -> Result<(), SQLError> {
    let input = (!matches!(source, SourcePlan::Join { .. }))
        .then(|| names.input(source))
        .flatten();
    let outer = input.as_ref();
    match source {
        SourcePlan::Table { .. } => {
            prepare_named_source(context, source, params, parent, prune, outer, memory)?;
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
                prune,
                names.with_names(),
                memory,
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
                prune,
                SourceScope {
                    outer: names.outer,
                    preceding: Some(&preceding),
                    inherited_lateral: *lateral,
                    retain_names: true,
                },
                memory,
            )?;
        }
        SourcePlan::Subquery { body, .. } => {
            prepare_query(context, body, params, parent, outer, memory)?;
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
    prune: Option<&ColumnPrune>,
    outer: Option<&RowSchema>,
    memory: &mut uqa_sql::plan::ExecutableMemory,
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
    let retained = context.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.ctes.runtime.cancellation,
    );
    let qualifier = alias.as_deref().unwrap_or(qualifier);
    let mut scope = parent.clone();
    let (mut query, aliases, optimize) = if let Some(cte) = parent.deferred_reference(name) {
        if !scope.cte_is_folded(name) {
            return Ok(());
        }
        let Some(query) = cte.body.query() else {
            return Ok(());
        };
        let query = query.copy_for_rewrite(&control)?;
        let aliases = super::table_source::production::output_aliases(
            &cte.columns,
            column_aliases,
            &control,
        )?;
        scope = parent.definition_scope(name).expect("declared CTE scope");
        (query, aliases, false)
    } else {
        if scope.is_visible_cte(name) {
            return Ok(());
        }
        let catalog = scope.catalog_read_view()?;
        let resolution = scope.relation_name_resolution_with_control(&control)?;
        let Some(view) = catalog.view_resolved_with_control(&resolution, name, &control)? else {
            return Ok(());
        };
        if view.kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            return Ok(());
        }
        let subject = if view.security_invoker() {
            scope.invoking_role()?.clone_with_control(&control)?
        } else {
            control.finish(
                catalog.view_owner_with_control(view.security.role_owner, &control)?,
                control.empty_reservation(),
            )?
        };
        scope = {
            let scoped = scope.enter_privilege_subject_with_control(subject, &control)?;
            (*scoped).clone()
        };
        (
            view.query.copy_for_rewrite(&control)?,
            super::table_source::production::output_aliases(
                view.output_columns.as_deref().unwrap_or_default(),
                column_aliases,
                &control,
            )?,
            true,
        )
    };
    let _stored = optimize
        .then(uqa_sql::semantics::sets::placement_scope::SetPlacementScope::stored_definition);
    if optimize {
        query = super::table_source::production::restore_layouts(
            context, query, params, &scope, outer, &control,
        )?;
    }
    if let Some(projection) = prune.and_then(|prune| prune.get(qualifier)) {
        query = super::table_source::production::prune_outputs(
            context,
            query,
            &aliases,
            projection,
            super::table_source::production::QueryBinding {
                params,
                scope: &scope,
                outer,
            },
            &control,
        )?;
    }
    let query = finish_named_source(context, query, optimize, params, &scope, outer, &control)?;
    let (query, attached) = query.into_parts();
    *executable_query = Some(query);
    memory.absorb(attached);
    Ok(())
}

fn finish_named_source<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    query: uqa_sql::plan::MutableQueryPlan,
    optimize: bool,
    params: &[SQLParam],
    scope: &CteScope<S>,
    outer: Option<&RowSchema>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<OwnedQueryPlan, SQLError> {
    let query = if optimize {
        context
            .ctes
            .rewrites
            .optimize_retained_query(query, control)?
    } else {
        OwnedQueryPlan::from_produced(query.finish(control)?, control)?
    };
    prepare_owned(context, query, params, scope, outer)
}
