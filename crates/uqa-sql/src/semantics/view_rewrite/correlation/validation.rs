//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public expression validation uses the admitted DML namespace and keeps the existing diagnostic order.

use super::{
    ids, schemas, AutomaticViewLayer, ConflictActionPlan, CorrelatedDmlContext, DeletePlan,
    ExpressionScope, InsertPlan, MergePlan, MergeWhenPlan, QueryPlan, RowSchema, SQLError,
    ScalarExpr, UpdatePlan, ViewRewriteBindings, ViewRewriteContext,
};
use uqa_core::memory::ProductionControl;

fn validate_expression(
    input: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    super::super::validation::production::reference_columns(
        input,
        &|name| super::super::layer_column(layer, name).is_some(),
        scope,
        control,
    )
}

pub(super) fn validate(
    context: CorrelatedDmlContext<'_>,
    queries: &[QueryPlan],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (outer, _) = schemas::outer_schema(
        context.services,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
        control,
    )?;
    let scope = schemas::analysis_scope(
        context.services,
        context.ctes,
        queries,
        context.inherited_ctes,
        control,
    )?;
    for id in context.ids {
        control.check()?;
        let query = queries.get(*id).ok_or_else(|| {
            SQLError::Internal(format!("DML scalar subquery slot {id} is out of bounds"))
        })?;
        crate::binding::analyze_query_plan_schema_with_control(
            context.services.routines,
            query,
            context.params,
            scope.copy_with_control(control)?,
            Some(&outer),
            control,
        )?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_update_expressions(
    services: ViewRewriteContext<'_>,
    plan: &UpdatePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[crate::SQLParam],
    inherited_ctes: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::update(plan, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::returning(&plan.returning, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        for expression in assignment.expressions() {
            validate_expression(expression, layer, ordinary_scope, control)?;
        }
    }
    if let Some(predicate) = &plan.predicate {
        validate_expression(predicate, layer, ordinary_scope, control)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_expression(&projection.expr, layer, returning_scope, control)?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_delete_expressions(
    services: ViewRewriteContext<'_>,
    plan: &DeletePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[crate::SQLParam],
    inherited_ctes: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::delete(plan, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::returning(&plan.returning, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = &plan.predicate {
        validate_expression(predicate, layer, ordinary_scope, control)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_expression(&projection.expr, layer, returning_scope, control)?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "validates every MERGE action against the public view schema"
)]
pub(in crate::semantics::view_rewrite) fn validate_merge_expressions(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    layer: &AutomaticViewLayer,
    source: &RowSchema,
    params: &[crate::SQLParam],
    inherited_ctes: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::merge_matched(plan, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::merge_target_only(plan, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::returning(&plan.returning, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    validate_expression(&plan.join_condition, layer, matched_scope, control)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_expression(condition, layer, matched_scope, control)?;
                }
                for assignment in assignments {
                    for expression in assignment.expressions() {
                        validate_expression(expression, layer, matched_scope, control)?;
                    }
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    validate_expression(condition, layer, matched_scope, control)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_expression(condition, layer, target_only_scope, control)?;
                }
                for assignment in assignments {
                    for expression in assignment.expressions() {
                        validate_expression(expression, layer, target_only_scope, control)?;
                    }
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    validate_expression(condition, layer, target_only_scope, control)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        validate_expression(&projection.expr, layer, returning_scope, control)?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_insert_expressions(
    services: ViewRewriteContext<'_>,
    plan: &InsertPlan,
    layer: &AutomaticViewLayer,
    params: &[crate::SQLParam],
    inherited_ctes: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: true,
            ctes: &plan.ctes,
            ids: &ids::conflict(plan, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    validate(
        CorrelatedDmlContext {
            inherited_ctes,
            services,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &ids::returning(&plan.returning, control)?,
            params,
        },
        &plan.subqueries,
        control,
    )?;
    if let Some(conflict) = &plan.on_conflict {
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            let scope = ExpressionScope {
                target_qualifier: &plan.target_qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: true,
            };
            for assignment in assignments {
                for expression in assignment.expressions() {
                    validate_expression(expression, layer, scope, control)?;
                }
            }
            if let Some(predicate) = predicate {
                validate_expression(predicate, layer, scope, control)?;
            }
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        validate_expression(&projection.expr, layer, scope, control)?;
    }
    Ok(())
}
