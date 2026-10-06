//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::updatability::merge_into_materialized_view;
use super::{
    add_check_option, automatic_view_layer, combine_view_predicate, dml_analysis_scope,
    duplicate_assignment, duplicate_insert_column, finalize_source_returning,
    merge_matched_subquery_ids, merge_target_only_subquery_ids, merge_view_target_path,
    returning_subquery_ids, rewrite_correlated_dml_context, rewrite_existing_view_checks,
    rewrite_merge_returning, rewrite_target_expression, validate_mapped_columns,
    validate_merge_expressions, validate_merge_targets, validate_public_merge_contract,
    validate_public_merge_targets, validate_writable_columns, writable_column, AutomaticViewLayer,
    BTreeSet, ColumnWrite, CorrelatedDmlContext, ExpressionScope, LayerPrivileges, MergePlan,
    MergeViewTargetPath, MergeWhenPlan, SQLError, StoredViewKind, ViewRewriteContext,
};

/// The layer of view `view` that a MERGE, for which `merge_view_target_path` chose the automatic rewrite, passes through.
fn merge_layer(
    services: ViewRewriteContext<'_>,
    view: &str,
) -> Result<AutomaticViewLayer, SQLError> {
    automatic_view_layer(services, view)?.ok_or_else(|| {
        SQLError::Internal(format!(
            "automatic MERGE rewrite selected for view `{view}`, which cannot be rewritten"
        ))
    })
}

/// The columns of `layer` that the actions of `plan` insert or update: those an `INSERT` names, or the leading columns, one for each value, and those an `UPDATE` sets.
fn merge_modified_columns<'a>(layer: &'a AutomaticViewLayer, plan: &'a MergePlan) -> Vec<&'a str> {
    let mut modified = Vec::new();
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() {
                    modified.extend(
                        layer
                            .columns
                            .iter()
                            .take(values.len())
                            .map(|column| column.name.as_str()),
                    );
                } else {
                    modified.extend(columns.iter().map(|target| target.column.as_str()));
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => modified.extend(
                assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names()),
            ),
            _ => {}
        }
    }
    modified
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn rewrite_merge_to_base(
    services: ViewRewriteContext<'_>,
    statement: &MergePlan,
    params: &[crate::SQLParam],
    inherited_ctes: Option<&super::CteScope>,
) -> Result<MergePlan, SQLError> {
    if services.catalog.target_view_kind(&statement.target)? == Some(StoredViewKind::Materialized) {
        return Err(merge_into_materialized_view(&statement.target));
    }
    let analysis_scope = dml_analysis_scope(
        services,
        &statement.ctes,
        &statement.subqueries,
        inherited_ctes,
    )?;
    let source_schema = crate::semantics::view_rewrite::context::analyze_source_plan_schema(
        services,
        &statement.source,
        params,
        &analysis_scope,
        None,
    )?;
    validate_public_merge_targets(services, statement)?;
    validate_public_merge_contract(services, statement, &source_schema)?;
    if merge_view_target_path(services, statement)? != MergeViewTargetPath::AutomaticRewrite {
        return Err(SQLError::Internal(
            "automatic MERGE rewrite selected for a view-trigger target".into(),
        ));
    }
    let initial_layer = merge_layer(services, &statement.target)?;
    validate_merge_targets(&initial_layer, statement)?;
    validate_merge_expressions(
        services,
        statement,
        &initial_layer,
        &source_schema,
        params,
        inherited_ctes,
    )?;
    let mut initial_layer = Some(initial_layer);

    let mut plan = statement.clone();
    let mut privileges = LayerPrivileges::new();
    plan.target_privilege_subject = Some(privileges.check(
        services.authorization,
        &plan.target,
        plan.target_privilege_subject.as_ref(),
        || crate::semantics::view_privileges::ensure_merge(services.authorization, &plan),
    )?);
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    loop {
        if !visited.is_empty()
            && merge_view_target_path(services, &plan)? == MergeViewTargetPath::ViewTriggers
        {
            break;
        }
        let layer = match initial_layer.take() {
            Some(layer) => layer,
            None => merge_layer(services, &plan.target)?,
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        validate_merge_targets(&layer, &plan)?;
        validate_writable_columns(
            &layer,
            merge_modified_columns(&layer, &plan),
            ColumnWrite::Merge,
        )?;
        if visited.len() > 1 {
            plan.target_privilege_subject = Some(privileges.check(
                services.authorization,
                &plan.target,
                plan.target_privilege_subject.as_ref(),
                || crate::semantics::view_privileges::ensure_merge(services.authorization, &plan),
            )?);
        }

        let matched_subqueries = merge_matched_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: Some(&source_schema),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &matched_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let target_only_subqueries = merge_target_only_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &target_only_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subqueries = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: Some(&source_schema),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let matched_scope = ExpressionScope {
            target_qualifier: &plan.target_qualifier,
            returning_aliases: None,
            source: Some(&source_schema),
            include_excluded: false,
        };
        let target_only_scope = ExpressionScope {
            source: None,
            ..matched_scope
        };
        rewrite_target_expression(
            services,
            &mut plan.join_condition,
            &layer,
            matched_scope,
            &mut plan.subqueries,
        )?;
        if let Some(predicate) = &mut plan.target_predicate {
            rewrite_target_expression(
                services,
                predicate,
                &layer,
                target_only_scope,
                &mut plan.subqueries,
            )?;
        }
        for clause in &mut plan.when_clauses {
            match clause {
                MergeWhenPlan::UpdateMatched {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            services,
                            condition,
                            &layer,
                            matched_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                    for assignment in assignments.iter_mut() {
                        for expression in assignment.expressions_mut() {
                            rewrite_target_expression(
                                services,
                                expression,
                                &layer,
                                matched_scope,
                                &mut plan.subqueries,
                            )?;
                        }
                        for target in assignment.target.targets_mut() {
                            target.column =
                                writable_column(&layer, &target.column, ColumnWrite::Merge)?;
                        }
                    }
                    validate_mapped_columns(
                        &assignments
                            .iter()
                            .flat_map(|assignment| assignment.target.targets().iter().cloned())
                            .collect::<Vec<_>>(),
                        duplicate_assignment,
                    )?;
                }
                MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::NothingMatched { condition } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            services,
                            condition,
                            &layer,
                            matched_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                }
                MergeWhenPlan::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            services,
                            condition,
                            &layer,
                            target_only_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                    for assignment in assignments.iter_mut() {
                        for expression in assignment.expressions_mut() {
                            rewrite_target_expression(
                                services,
                                expression,
                                &layer,
                                target_only_scope,
                                &mut plan.subqueries,
                            )?;
                        }
                        for target in assignment.target.targets_mut() {
                            target.column =
                                writable_column(&layer, &target.column, ColumnWrite::Merge)?;
                        }
                    }
                    validate_mapped_columns(
                        &assignments
                            .iter()
                            .flat_map(|assignment| assignment.target.targets().iter().cloned())
                            .collect::<Vec<_>>(),
                        duplicate_assignment,
                    )?;
                }
                MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            services,
                            condition,
                            &layer,
                            target_only_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                }
                MergeWhenPlan::InsertNotMatched {
                    columns, values, ..
                } => {
                    let supplied_columns = if columns.is_empty() {
                        layer
                            .columns
                            .iter()
                            .take(values.len())
                            .map(|column| column.name.clone().into())
                            .collect::<Vec<_>>()
                    } else {
                        columns.clone()
                    };
                    *columns = supplied_columns
                        .into_iter()
                        .map(|mut target| {
                            target.column =
                                writable_column(&layer, &target.column, ColumnWrite::Merge)?;
                            Ok(target)
                        })
                        .collect::<Result<Vec<_>, SQLError>>()?;
                    validate_mapped_columns(columns, duplicate_insert_column)?;
                }
                MergeWhenPlan::NothingNotMatched { .. } => {}
            }
        }
        rewrite_existing_view_checks(
            services,
            &mut plan.view_checks,
            &layer,
            &plan.target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, boundaries) = rewrite_merge_returning(
            services,
            plan.returning,
            &layer,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &source_schema,
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.target_predicate = combine_view_predicate(
            services,
            plan.target_predicate,
            &layer,
            &plan.target_qualifier,
            &mut plan.subqueries,
        )?;
        add_check_option(
            services,
            &mut plan.view_checks,
            &layer,
            &plan.target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.target = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        if !super::context::target_is_view(services, &plan.target)? {
            break;
        }
    }
    plan.returning = finalize_source_returning(
        services,
        &plan.target,
        plan.returning,
        Some(&source_schema),
        &source_star_boundaries,
    )?;
    privileges.finish()?;
    Ok(plan)
}
