//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve mutation execution paths using SQL view rewriting and execution-owned command loops.
use super::{insert::table::InsertPlanning, views::commands::SourceOutputPruning};
use crate::mutation::statement::context::MutationStatementContext;
use crate::query::CteScope;
use uqa_sql::{
    plan::{DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    semantics::{view_privileges, view_rewrite},
    SQLError, SQLParam, SQLResult,
};

pub fn run_insert<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    planning: InsertPlanning<'_>,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let rewrite = context.mutation.rules.views.rewrite;
    if let Some(kind) = rewrite.catalog.target_view_kind(&stmt.table)? {
        if kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            let _ = view_privileges::ensure_insert(rewrite.authorization, stmt)?;
            let relation = uqa_core::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if view_rewrite::has_instead_of_trigger(
            rewrite,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Insert,
        )? || uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.mutation.rules.rules.analysis,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Insert,
        )? {
            let _ = view_privileges::ensure_insert(rewrite.authorization, stmt)?;
            return super::views::commands::run_view_insert_inner(
                context,
                planning.prune_source_outputs,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let bindings = inherited_ctes
            .map(crate::query::binding::binding_context)
            .transpose()?
            .map(uqa_sql::binding::snapshot::BindingSnapshot::from);
        let rewritten =
            view_rewrite::rewrite_insert_to_base(rewrite, stmt, params, bindings.as_ref())?;
        return run_insert(context, planning, &rewritten, params, inherited_ctes);
    }
    crate::mutation::insert::table::run_table_insert(
        context,
        planning,
        stmt,
        params,
        inherited_ctes,
    )
}

pub fn run_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let rewrite = context.mutation.rules.views.rewrite;
    if let Some(kind) = rewrite.catalog.target_view_kind(&stmt.table)? {
        if kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            let _ = view_privileges::ensure_update(rewrite.authorization, stmt)?;
            let relation = uqa_core::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if view_rewrite::has_instead_of_trigger(
            rewrite,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Update,
        )? || uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.mutation.rules.rules.analysis,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Update,
        )? {
            let _ = view_privileges::ensure_update(rewrite.authorization, stmt)?;
            return super::views::commands::run_view_update_inner(
                context,
                prune,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let bindings = inherited_ctes
            .map(crate::query::binding::binding_context)
            .transpose()?
            .map(uqa_sql::binding::snapshot::BindingSnapshot::from);
        let rewritten =
            view_rewrite::rewrite_update_to_base(rewrite, stmt, params, bindings.as_ref())?;
        return run_update(context, prune, &rewritten, params, inherited_ctes);
    }
    crate::mutation::update::table::run_table_update(context, stmt, params, inherited_ctes)
}

pub fn run_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let rewrite = context.mutation.rules.views.rewrite;
    if let Some(kind) = rewrite.catalog.target_view_kind(&stmt.table)? {
        if kind == uqa_sql::catalog::view::StoredViewKind::Materialized {
            let _ = view_privileges::ensure_delete(rewrite.authorization, stmt)?;
            let relation = uqa_core::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if view_rewrite::has_instead_of_trigger(
            rewrite,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Delete,
        )? || uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.mutation.rules.rules.analysis,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Delete,
        )? {
            let _ = view_privileges::ensure_delete(rewrite.authorization, stmt)?;
            return super::views::commands::run_view_delete_inner(
                context,
                prune,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let bindings = inherited_ctes
            .map(crate::query::binding::binding_context)
            .transpose()?
            .map(uqa_sql::binding::snapshot::BindingSnapshot::from);
        let rewritten =
            view_rewrite::rewrite_delete_to_base(rewrite, stmt, params, bindings.as_ref())?;
        return run_delete(context, prune, &rewritten, params, inherited_ctes);
    }
    crate::mutation::delete::run_table_delete(context, stmt, params, inherited_ctes)
}

pub fn run_merge<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    stmt: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let rewrite = context.mutation.rules.views.rewrite;
    if rewrite.catalog.target_view_kind(&stmt.target)?.is_some() {
        let scope = super::merge::analysis::merge_analysis_scope(
            context.mutation.scopes,
            stmt,
            inherited_ctes,
        )?;
        let bindings = crate::query::binding::binding_context(&scope)?;
        let source = uqa_sql::binding::analyze_source_plan_schema(
            rewrite.catalog,
            &stmt.source,
            params,
            &bindings,
            None,
        )?;
        view_rewrite::validate_public_merge_targets(rewrite, stmt)?;
        view_rewrite::validate_public_merge_contract(rewrite, stmt, &source)?;
        return match view_rewrite::merge_view_target_path(rewrite, stmt)? {
            view_rewrite::MergeViewTargetPath::AutomaticRewrite => {
                let inherited = inherited_ctes
                    .map(crate::query::binding::binding_context)
                    .transpose()?
                    .map(uqa_sql::binding::snapshot::BindingSnapshot::from);
                let rewritten =
                    view_rewrite::rewrite_merge_to_base(rewrite, stmt, params, inherited.as_ref())?;
                run_merge(context, prune, &rewritten, params, inherited_ctes)
            }
            view_rewrite::MergeViewTargetPath::ViewTriggers => {
                let _ = view_privileges::ensure_merge(rewrite.authorization, stmt)?;
                super::merge::views::run_view_merge(context, prune, stmt, params, inherited_ctes)
            }
        };
    }
    super::merge::table::run_table_merge(context, stmt, params, inherited_ctes)
}
