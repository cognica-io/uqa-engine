//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-lock discovery borrows lexical and stored query owners and admits only selected metadata.

use super::RowLockContext;
use crate::row_locks::binding::{bind_relation, lock_descendants_with_control, RelationBinding};
use uqa_core::{
    memory::{MemoryReservation, ProductionControl},
    RelationName,
};
use uqa_sql::{
    binding::production::BindingNameSet,
    catalog::resolution::RelationResolution,
    plan::{CommandPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    SQLError,
};
mod names;
use super::targets::lock_table_function_relations;
use names::{LocalNames, VisibleNames};

type BoundRelation = RelationBinding<(&'static str, Option<MemoryReservation>)>;

fn resolve_binding<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    name: &str,
    bound: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<BoundRelation>, SQLError> {
    let resolution = context
        .catalog
        .resolve_relation_with_control(name, bound, control)?;
    let RelationResolution::Found(canonical, _) = &*resolution else {
        return Ok(None);
    };
    let object_id = context
        .catalog
        .relation_object_id_with_control(canonical, control)?;
    let (resolution, memory) = resolution.into_parts();
    let RelationResolution::Found(name, kind) = resolution else {
        unreachable!()
    };
    Ok(Some(RelationBinding {
        name,
        object_id,
        value: (kind, memory),
    }))
}

/// Acquire `PostgreSQL` `AccessShareLock` equivalents for every concrete table referenced by a query. The locks are transaction-scoped, so a cursor declaration keeps its bound relations alive until commit while ordinary persistent statements keep DDL from changing a source during execution.
pub fn lock_query_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
) -> Result<(), SQLError> {
    let resources = context.scopes.production_resources()?;
    let control = resources.control();
    control.check()?;
    let mut locked = BindingNameSet::new();
    let mut visiting_views = BindingNameSet::new();
    let transition_relations = context
        .scopes
        .transition_relation_names_with_control(&control)?;
    lock_query_plan_relations(
        context,
        query,
        &transition_relations,
        &mut locked,
        &mut visiting_views,
        &control,
    )
}

/// Hold the explained statement's relation identities before collecting physical metadata or executing ANALYZE.
pub fn lock_explained_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => lock_query_relations(context, query),
        UnifiedPlan::Command(command) => match command.as_ref() {
            CommandPlan::Explain { body, .. } => lock_explained_relations(context, body),
            CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => lock_query_relations(context, query),
            _ => {
                let resources = context.scopes.production_resources()?;
                let control = resources.control();
                control.check()?;
                lock_command_plan_relations(
                    context,
                    command,
                    &context
                        .scopes
                        .transition_relation_names_with_control(&control)?,
                    &mut BindingNameSet::new(),
                    &mut BindingNameSet::new(),
                    &control,
                )
            }
        },
    }
}

fn lock_query_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
    inherited_ctes: &dyn VisibleNames,
    locked: &mut BindingNameSet,
    visiting_views: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut visible_ctes = LocalNames::new(inherited_ctes, *control);
    for cte in &query.ctes {
        let mut definition_scope = LocalNames::new(&visible_ctes, *control);
        if cte.recursive {
            definition_scope.insert(&cte.name)?;
        }
        lock_cte_plan_relations(
            context,
            &cte.body,
            &definition_scope,
            locked,
            visiting_views,
            control,
        )?;
        visible_ctes.insert(&cte.name)?;
    }
    lock_relational_plan_relations(
        context,
        &query.root,
        &visible_ctes,
        query.relations_bound,
        locked,
        visiting_views,
        control,
    )
}

fn lock_cte_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    body: &uqa_sql::plan::CtePlanBody,
    inherited: &dyn VisibleNames,
    locked: &mut BindingNameSet,
    visiting: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match body {
        uqa_sql::plan::CtePlanBody::Query(query) => {
            lock_query_plan_relations(context, query, inherited, locked, visiting, control)
        }
        uqa_sql::plan::CtePlanBody::Command(command) => {
            lock_command_plan_relations(context, command, inherited, locked, visiting, control)
        }
    }
}

fn lock_command_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    command: &CommandPlan,
    inherited: &dyn VisibleNames,
    locked: &mut BindingNameSet,
    visiting: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let (bound, target_bound, descendants) = match command {
        CommandPlan::Insert(plan) => (plan.relations_bound, plan.target_relation_bound, false),
        CommandPlan::Update(plan) => (
            plan.relations_bound,
            plan.target_relation_bound,
            plan.include_descendants,
        ),
        CommandPlan::Delete(plan) => (
            plan.relations_bound,
            plan.target_relation_bound,
            plan.include_descendants,
        ),
        CommandPlan::Merge(plan) => (false, false, plan.include_descendants),
        _ => (false, false, false),
    };
    if let Some(target) = command.mutation_target() {
        let mode = crate::row_locks::RelationLockMode::RowExclusive;
        if let Some(binding) = bind_relation(
            context.session,
            mode,
            false,
            || resolve_binding(context, target, target_bound, control),
            |_| Ok(()),
        )? {
            if binding.value.0 == "table" {
                let names = context.catalog.hierarchy_scan_tables_with_control(
                    &binding.name,
                    descendants,
                    control,
                )?;
                lock_descendants_with_control(
                    context.catalog,
                    context.session,
                    names
                        .iter()
                        .filter(|name| *name != &binding.name)
                        .map(String::as_str),
                    mode,
                    false,
                    control,
                )?;
            }
        }
    }
    let mut visible = LocalNames::new(inherited, *control);
    if command.ctes().iter().any(|cte| cte.recursive) {
        for cte in command.ctes() {
            visible.insert(&cte.name)?;
        }
    }
    for cte in command.ctes() {
        lock_cte_plan_relations(context, &cte.body, &visible, locked, visiting, control)?;
        visible.insert(&cte.name)?;
    }
    for query in command.query_inputs() {
        lock_query_plan_relations(context, query, &visible, locked, visiting, control)?;
    }
    if let Some(source) = command.source_input() {
        lock_source_plan_relations(context, source, &visible, bound, locked, visiting, control)?;
    }
    Ok(())
}

fn lock_relational_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    plan: &RelationalPlan,
    visible_ctes: &dyn VisibleNames,
    relations_bound: bool,
    locked: &mut BindingNameSet,
    visiting_views: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                lock_source_plan_relations(
                    context,
                    source,
                    visible_ctes,
                    relations_bound,
                    locked,
                    visiting_views,
                    control,
                )?;
            }
            for subquery in &block.subqueries {
                lock_query_plan_relations(
                    context,
                    subquery,
                    visible_ctes,
                    locked,
                    visiting_views,
                    control,
                )?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            lock_query_plan_relations(
                context,
                left,
                visible_ctes,
                locked,
                visiting_views,
                control,
            )?;
            lock_query_plan_relations(
                context,
                right,
                visible_ctes,
                locked,
                visiting_views,
                control,
            )?;
            for subquery in subqueries {
                lock_query_plan_relations(
                    context,
                    subquery,
                    visible_ctes,
                    locked,
                    visiting_views,
                    control,
                )?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                lock_query_plan_relations(
                    context,
                    subquery,
                    visible_ctes,
                    locked,
                    visiting_views,
                    control,
                )?;
            }
            Ok(())
        }
    }
}

fn lock_source_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &SourcePlan,
    visible_ctes: &dyn VisibleNames,
    relations_bound: bool,
    locked: &mut BindingNameSet,
    visiting_views: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table {
            name,
            include_descendants,
            ..
        } => {
            if uqa_core::RelationIdentity::borrowed_reference(name).is_some_and(|reference| {
                reference.schema.is_none() && visible_ctes.contains(reference.name)
            }) {
                return Ok(());
            }
            lock_named_query_relation(
                context,
                name,
                relations_bound,
                *include_descendants,
                locked,
                visiting_views,
                control,
            )
        }
        SourcePlan::Join { left, right, .. } => {
            lock_source_plan_relations(
                context,
                left,
                visible_ctes,
                relations_bound,
                locked,
                visiting_views,
                control,
            )?;
            lock_source_plan_relations(
                context,
                right,
                visible_ctes,
                relations_bound,
                locked,
                visiting_views,
                control,
            )
        }
        SourcePlan::Subquery { body, .. } => {
            lock_query_plan_relations(context, body, visible_ctes, locked, visiting_views, control)
        }
        SourcePlan::Function { relations, .. } => lock_table_function_relations(
            context,
            relations.as_ref(),
            relations_bound,
            locked,
            control,
        ),
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                lock_table_function_relations(
                    context,
                    function.relations.as_ref(),
                    relations_bound,
                    locked,
                    control,
                )?;
            }
            Ok(())
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Values { .. } => Ok(()),
    }
}

fn lock_named_query_relation<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    name: &str,
    relations_bound: bool,
    include_descendants: bool,
    locked: &mut BindingNameSet,
    visiting_views: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let Some(binding) = bind_relation(
        context.session,
        crate::row_locks::RelationLockMode::AccessShare,
        false,
        || resolve_binding(context, name, relations_bound, control),
        |_| Ok(()),
    )?
    else {
        return Ok(());
    };
    let canonical = &binding.name;
    locked.insert_with_control(canonical, control)?;
    if let Some(relation) = uqa_sql::catalog::SystemRelation::from_qualified_name(canonical) {
        for source in relation.view_sources() {
            lock_named_query_relation(
                context,
                &control.format(format_args!("{}.{}", source.namespace(), source.name()))?,
                true,
                false,
                locked,
                visiting_views,
                control,
            )?;
        }
        return Ok(());
    }
    match binding.value.0 {
        "table" => {
            let names = context.catalog.hierarchy_scan_tables_with_control(
                canonical,
                include_descendants,
                control,
            )?;
            lock_descendants_with_control(
                context.catalog,
                context.session,
                names
                    .iter()
                    .filter(|child| *child != canonical)
                    .map(String::as_str),
                crate::row_locks::RelationLockMode::AccessShare,
                false,
                control,
            )
        }
        "view" => {
            let found =
                context
                    .catalog
                    .with_view_plan_with_control(canonical, control, &mut |view| {
                        if !visiting_views.insert_with_control(canonical, control)? {
                            return Err(SQLError::Internal(
                                control
                                    .format(format_args!(
                                        "view `{canonical}` has a recursive relation dependency"
                                    ))?
                                    .into_parts()
                                    .0,
                            ));
                        }
                        let result = lock_query_plan_relations(
                            context,
                            view,
                            &BindingNameSet::new(),
                            locked,
                            visiting_views,
                            control,
                        );
                        visiting_views.remove(canonical);
                        result
                    })?;
            if found {
                Ok(())
            } else {
                Err(SQLError::Internal(
                    control
                        .format(format_args!(
                            "resolved query view `{canonical}` disappeared before locking"
                        ))?
                        .into_parts()
                        .0,
                ))
            }
        }
        _ => Ok(()),
    }
}
