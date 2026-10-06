//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    add_check_option, bind_unqualified_source_positions, canonical_view_name,
    combine_view_predicate, delete_ordinary_subquery_ids, dml_source_schema, dml_target_width,
    duplicate_assignment, finalize_source_returning, instead_of_trigger_definition,
    next_rewritten_layer, preserve_view_rule_returning, record_view_rule_relation,
    returning_subquery_ids, rewritable_layer, rewrite_correlated_dml_context,
    rewrite_existing_view_checks, rewrite_returning, rewrite_target_expression,
    update_ordinary_subquery_ids, validate_delete_expressions, validate_direct_view_rule_path,
    validate_mapped_columns, validate_public_delete_contract, validate_public_update_contract,
    validate_public_view_targets, validate_update_expressions, validate_update_targets,
    validate_writable_columns, view_not_updatable, writable_column, AssignmentPlan, BTreeSet,
    ColumnWrite, CorrelatedDmlContext, DeletePlan, ExpressionScope, LayerPrivileges,
    NotUpdatableReason, SQLError, TriggerEvent, UpdatePlan, ViewCommand, ViewRewriteContext,
    ViewRuleUpdatePlan,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn rewrite_update_to_base(
    services: ViewRewriteContext<'_>,
    statement: &UpdatePlan,
    params: &[crate::SQLParam],
    inherited_ctes: Option<&super::CteScope>,
) -> Result<UpdatePlan, SQLError> {
    validate_public_view_targets(
        services,
        &statement.table,
        statement
            .assignments
            .iter()
            .flat_map(|assignment| assignment.target.column_names()),
    )?;
    let source_schema = dml_source_schema(
        services,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
        inherited_ctes,
    )?;
    validate_public_update_contract(services, statement, source_schema.as_ref())?;
    let view = canonical_view_name(services, &statement.table)?;
    validate_direct_view_rule_path(
        services,
        &view,
        crate::ast::RuleEvent::Update,
        ViewCommand::Update,
    )?;
    let initial_layer = rewritable_layer(services, &view, ViewCommand::Update)?;
    if !initial_layer.has_writable_column() {
        return Err(view_not_updatable(
            &view,
            ViewCommand::Update,
            NotUpdatableReason::NoUpdatableColumns,
        ));
    }
    validate_update_targets(&initial_layer, statement)?;
    let mut initial_layer = Some(initial_layer);
    let mut plan = statement.clone();
    let mut privileges = LayerPrivileges::new();
    plan.target_privilege_subject = Some(privileges.check(
        services.authorization,
        &plan.table,
        plan.target_privilege_subject.as_ref(),
        || crate::semantics::view_privileges::ensure_update(services.authorization, &plan),
    )?);
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    let mut rewrite_suppressed = false;
    loop {
        // An underlying view with an INSTEAD OF trigger ends the rewrite, since `RewriteQuery` rewrites only a view without one: the trigger performs the UPDATE on that view.
        if !visited.is_empty()
            && !rewrite_suppressed
            && instead_of_trigger_definition(services, &plan.table, TriggerEvent::Update)?
        {
            break;
        }
        let Some(layer) = next_rewritten_layer(
            services,
            &plan.table,
            &mut initial_layer,
            rewrite_suppressed,
            crate::ast::RuleEvent::Update,
            ViewCommand::Update,
        )?
        else {
            break;
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                services,
                &mut plan.view_rule_relations,
                &layer,
                crate::ast::RuleEvent::Update,
            )?
        };
        let layer_suppresses = has_view_rules
            && super::context::relation_suppresses_original_query(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Update,
            )?;
        if visited.len() > 1 && !rewrite_suppressed && !layer_suppresses {
            if !layer.has_writable_column() {
                return Err(view_not_updatable(
                    &layer.canonical_name,
                    ViewCommand::Update,
                    NotUpdatableReason::NoUpdatableColumns,
                ));
            }
            plan.target_privilege_subject = Some(privileges.check(
                services.authorization,
                &plan.table,
                plan.target_privilege_subject.as_ref(),
                || crate::semantics::view_privileges::ensure_update(services.authorization, &plan),
            )?);
        }
        if has_view_rules
            && super::context::relation_has_returning_provider(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Update,
            )?
        {
            preserve_view_rule_returning(
                &mut plan.view_rule_returning,
                &layer.canonical_name,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            );
        }
        if has_view_rules {
            plan.view_rule_update_plans.push(ViewRuleUpdatePlan {
                relation: layer.canonical_name.clone(),
                assigned_columns: plan
                    .assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names())
                    .map(str::to_owned)
                    .collect(),
                input_columns: Vec::new(),
            });
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_update_expressions(
                services,
                &plan,
                &layer,
                source_schema.as_ref(),
                params,
                inherited_ctes,
            )?;
        }
        let ordinary_subquery_ids = update_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &ordinary_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subquery_ids = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let ordinary_scope = ExpressionScope {
            target_qualifier: &target_qualifier,
            returning_aliases: None,
            source: source_schema.as_ref(),
            include_excluded: false,
        };
        if !layer_suppresses && !rewrite_suppressed {
            validate_writable_columns(
                &layer,
                plan.assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names()),
                ColumnWrite::Update,
            )?;
        }
        for AssignmentPlan { target, value } in &mut plan.assignments {
            for expression in target.expressions_mut() {
                rewrite_target_expression(
                    services,
                    expression,
                    &layer,
                    ordinary_scope,
                    &mut plan.subqueries,
                )?;
            }
            rewrite_target_expression(
                services,
                value,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
            if !layer_suppresses && !rewrite_suppressed {
                for target in target.targets_mut() {
                    target.column = writable_column(&layer, &target.column, ColumnWrite::Update)?;
                }
            }
        }
        let mapped = plan
            .assignments
            .iter()
            .flat_map(|assignment| assignment.target.targets().iter().cloned())
            .collect::<Vec<_>>();
        validate_mapped_columns(&mapped, duplicate_assignment)?;
        if let Some(predicate) = &mut plan.predicate {
            rewrite_target_expression(
                services,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        rewrite_existing_view_checks(
            services,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, boundaries) = rewrite_returning(
            services,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            source_schema.as_ref(),
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.predicate = combine_view_predicate(
            services,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        add_check_option(
            services,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        rewrite_suppressed |= layer_suppresses;
        if !super::context::target_is_view(services, &plan.table)? {
            break;
        }
    }
    let input_columns = plan
        .assignments
        .iter()
        .flat_map(|assignment| assignment.target.column_names())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for update_plan in &mut plan.view_rule_update_plans {
        update_plan.input_columns.clone_from(&input_columns);
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(services, &plan.table)?;
        for assignment in &mut plan.assignments {
            for expression in assignment.expressions_mut() {
                bind_unqualified_source_positions(expression, source, target_width);
            }
        }
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        services,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    privileges.finish()?;
    Ok(plan)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn rewrite_delete_to_base(
    services: ViewRewriteContext<'_>,
    statement: &DeletePlan,
    params: &[crate::SQLParam],
    inherited_ctes: Option<&super::CteScope>,
) -> Result<DeletePlan, SQLError> {
    let source_schema = dml_source_schema(
        services,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
        inherited_ctes,
    )?;
    validate_public_delete_contract(services, statement, source_schema.as_ref())?;
    let mut plan = statement.clone();
    let mut privileges = LayerPrivileges::new();
    plan.target_privilege_subject = Some(privileges.check(
        services.authorization,
        &plan.table,
        plan.target_privilege_subject.as_ref(),
        || crate::semantics::view_privileges::ensure_delete(services.authorization, &plan),
    )?);
    let mut initial_layer = None;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    let mut rewrite_suppressed = false;
    loop {
        // An underlying view with an INSTEAD OF trigger ends the rewrite, since `RewriteQuery` rewrites only a view without one: the trigger performs the DELETE on that view.
        if !visited.is_empty()
            && !rewrite_suppressed
            && instead_of_trigger_definition(services, &plan.table, TriggerEvent::Delete)?
        {
            break;
        }
        let Some(layer) = next_rewritten_layer(
            services,
            &plan.table,
            &mut initial_layer,
            rewrite_suppressed,
            crate::ast::RuleEvent::Delete,
            ViewCommand::Delete,
        )?
        else {
            break;
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                services,
                &mut plan.view_rule_relations,
                &layer,
                crate::ast::RuleEvent::Delete,
            )?
        };
        let layer_suppresses = has_view_rules
            && super::context::relation_suppresses_original_query(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Delete,
            )?;
        if visited.len() > 1 && !rewrite_suppressed && !layer_suppresses {
            plan.target_privilege_subject = Some(privileges.check(
                services.authorization,
                &plan.table,
                plan.target_privilege_subject.as_ref(),
                || crate::semantics::view_privileges::ensure_delete(services.authorization, &plan),
            )?);
        }
        if has_view_rules
            && super::context::relation_has_returning_provider(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Delete,
            )?
        {
            preserve_view_rule_returning(
                &mut plan.view_rule_returning,
                &layer.canonical_name,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            );
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_delete_expressions(
                services,
                &plan,
                &layer,
                source_schema.as_ref(),
                params,
                inherited_ctes,
            )?;
        }
        let ordinary_subquery_ids = delete_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &ordinary_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subquery_ids = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let ordinary_scope = ExpressionScope {
            target_qualifier: &target_qualifier,
            returning_aliases: None,
            source: source_schema.as_ref(),
            include_excluded: false,
        };
        if let Some(predicate) = &mut plan.predicate {
            rewrite_target_expression(
                services,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        let (returning, boundaries) = rewrite_returning(
            services,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            source_schema.as_ref(),
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.predicate = combine_view_predicate(
            services,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        rewrite_suppressed |= layer_suppresses;
        if !super::context::target_is_view(services, &plan.table)? {
            break;
        }
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(services, &plan.table)?;
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        services,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    privileges.finish()?;
    Ok(plan)
}
