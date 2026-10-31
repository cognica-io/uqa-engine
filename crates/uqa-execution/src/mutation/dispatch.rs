//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve mutation execution paths using SQL view rewriting and execution-owned command loops.
use super::insert::table::InsertPlanning;
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::MutationStatementContext;
use crate::query::CteScope;
use uqa_sql::{
    plan::{DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    semantics::{view_privileges, view_rewrite},
    SQLError, SQLParam,
};

pub fn run_insert<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    planning: InsertPlanning<'_>,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let rewrite = context.mutation.rules.views;
    let retained = context.mutation.scopes.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.mutation.scopes.cancellation(),
    );

    if let Some(kind) = rewrite
        .catalog
        .target_view_kind_with_control(&stmt.table, &control)?
    {
        if kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            let _ =
                view_privileges::ensure_insert_with_control(rewrite.authorization, stmt, &control)?;
            let relation = uqa_core::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if rewrite.catalog.has_trigger_definition_with_control(
            &stmt.table,
            uqa_sql::ast::TriggerTiming::InsteadOf,
            uqa_sql::ast::TriggerEvent::Insert,
            true,
            &control,
        )? || rewrite
            .catalog
            .capture_rules_with_control(&stmt.table, uqa_sql::ast::RuleEvent::Insert, &control)?
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none())?
        {
            let _ =
                view_privileges::ensure_insert_with_control(rewrite.authorization, stmt, &control)?;
            let expanded = set_returning_values_query(context, stmt, &control)?;
            let stmt = expanded.as_deref().map_or(stmt, expanded_insert);
            super::routine_calls::insert(context, stmt, params, inherited_ctes)?;
            return super::views::commands::run_view_insert_inner(
                context,
                planning.prune_source_outputs,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let bindings = inherited_ctes
            .map(|scope| crate::query::binding::binding_snapshot_with_control(scope, &control))
            .transpose()?;
        let rewritten = view_rewrite::rewrite_insert_owned(
            rewrite,
            stmt.copy_for_rewrite(&control)?,
            params,
            bindings
                .as_ref()
                .map(view_rewrite::ViewRewriteBindings::Controlled),
            &control,
        )?;
        let uqa_sql::plan::CommandPlan::Insert(plan) = &*rewritten else {
            unreachable!("rewritten INSERT")
        };
        return run_insert(context, planning, plan, params, inherited_ctes);
    }
    let expanded = set_returning_values_query(context, stmt, &control)?;
    crate::mutation::insert::table::run_table_insert(
        context,
        planning,
        expanded.as_deref().map_or(stmt, expanded_insert),
        params,
        inherited_ctes,
    )
}

/// A single VALUES row runs as a target list, so its set-returning calls insert one row per result; the INSERT then takes its `INSERT ... SELECT` form. The target's own rewrite rules decide whether they read such an entry.
fn set_returning_values_query<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &InsertPlan,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_sql::plan::MutableCommandPlan>, SQLError> {
    uqa_sql::semantics::insert_values::single_row_values_query_with_control(
        context.query.source.ctes.routines,
        stmt,
        || {
            uqa_sql::semantics::rules::analysis::relation_rule_row_columns_with_control(
                context.mutation.rules.rules.analysis,
                &stmt.table,
                uqa_sql::ast::RuleEvent::Insert,
                control,
            )
        },
        || {
            uqa_sql::semantics::insert_values::implicit_target_columns_with_control(
                context.mutation.rules.views,
                &stmt.table,
                control,
            )
        },
        control,
    )
}

fn expanded_insert(command: &uqa_sql::plan::CommandPlan) -> &InsertPlan {
    let uqa_sql::plan::CommandPlan::Insert(plan) = command else {
        unreachable!("INSERT VALUES expansion")
    };
    plan
}

pub fn run_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    super::rules::schedule::run_update(context, stmt, params, inherited_ctes)
}

pub fn run_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    super::rules::schedule::run_delete(context, stmt, params, inherited_ctes)
}

pub fn run_merge<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let rewrite = context.mutation.rules.views;
    let retained = context.mutation.scopes.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.mutation.scopes.cancellation(),
    );

    if rewrite
        .catalog
        .target_view_kind_with_control(&stmt.target, &control)?
        .is_some()
    {
        let scope = super::merge::analysis::merge_analysis_scope(
            context.mutation.scopes,
            stmt,
            inherited_ctes,
        )?;
        let bindings = crate::query::binding::binding_context_with_control(&scope, &control)?;
        let source = uqa_sql::binding::analyze_source_plan_schema_with_control(
            rewrite.routines,
            &stmt.source,
            params,
            bindings,
            None,
            &control,
        )?;
        view_rewrite::validate_public_merge_targets_with_control(rewrite, stmt, &control)?;
        view_rewrite::validate_public_merge_contract_with_control(
            rewrite, stmt, &source, &control,
        )?;
        return match view_rewrite::merge_view_target_path_with_control(rewrite, stmt, &control)? {
            view_rewrite::MergeViewTargetPath::AutomaticRewrite => {
                let inherited = inherited_ctes
                    .map(|scope| {
                        crate::query::binding::binding_snapshot_with_control(scope, &control)
                    })
                    .transpose()?;
                let rewritten = view_rewrite::rewrite_merge_owned(
                    rewrite,
                    stmt.copy_for_rewrite(&control)?,
                    params,
                    inherited
                        .as_ref()
                        .map(view_rewrite::ViewRewriteBindings::Controlled),
                    &control,
                )?;
                let uqa_sql::plan::CommandPlan::Merge(plan) = &*rewritten else {
                    unreachable!("rewritten MERGE")
                };
                run_merge(context, plan, params, inherited_ctes)
            }
            view_rewrite::MergeViewTargetPath::ViewTriggers => {
                let _ = view_privileges::ensure_merge_with_control(
                    rewrite.authorization,
                    stmt,
                    &control,
                )?;
                super::routine_calls::merge(context, stmt, params, inherited_ctes)?;
                super::merge::views::run_view_merge(context, stmt, params, inherited_ctes)
            }
        };
    }
    super::merge::table::run_table_merge(context, stmt, params, inherited_ctes)
}
