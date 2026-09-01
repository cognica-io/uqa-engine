//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    active_unconditional_instead_rule, add_check_option, automatic_view_layer,
    bind_unqualified_source_positions, combine_view_predicate, delete_ordinary_subquery_ids,
    dml_source_schema, dml_target_width, duplicate_assignment, finalize_source_returning,
    instead_of_trigger_definition, not_automatically_updatable, preserve_view_rule_returning,
    record_view_rule_relation, returning_subquery_ids, rewrite_correlated_dml_context,
    rewrite_existing_view_checks, rewrite_returning, rewrite_target_expression,
    update_ordinary_subquery_ids, validate_delete_expressions, validate_direct_view_rule_path,
    validate_mapped_columns, validate_public_delete_contract, validate_public_update_contract,
    validate_public_view_targets, validate_update_expressions, validate_update_targets,
    view_updatability, writable_column, AssignmentPlan, BTreeSet, CorrelatedDmlContext, DeletePlan,
    Engine, ExpressionScope, SQLError, TriggerEvent, UpdatePlan, ViewRuleUpdatePlan,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub(in crate::sql::dml) fn rewrite_update_to_base(
    engine: &Engine,
    statement: &UpdatePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<UpdatePlan, SQLError> {
    validate_public_view_targets(
        engine,
        &statement.table,
        statement
            .assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
    )?;
    let source_schema = dml_source_schema(
        engine,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
    )?;
    validate_public_update_contract(engine, statement, source_schema.as_ref())?;
    let Some(initial_layer) = automatic_view_layer(engine, &statement.table)? else {
        return Err(not_automatically_updatable(&statement.table, "UPDATE"));
    };
    validate_update_targets(&initial_layer, statement)?;
    validate_direct_view_rule_path(
        engine,
        &initial_layer.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
        "UPDATE",
    )?;
    if !view_updatability(engine, &statement.table)?
        .automatic
        .updatable
    {
        return Err(not_automatically_updatable(&statement.table, "UPDATE"));
    }
    let mut plan = statement.clone();
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    let mut rewrite_suppressed = false;
    loop {
        let Some(layer) = automatic_view_layer(engine, &plan.table)? else {
            if active_unconditional_instead_rule(
                engine,
                &plan.table,
                uqa_sql::ast::RuleEvent::Update,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "UPDATE"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        if !rewrite_suppressed {
            validate_direct_view_rule_path(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
                "UPDATE",
            )?;
        }
        if !rewrite_suppressed
            && visited.len() > 1
            && instead_of_trigger_definition(engine, &layer.canonical_name, TriggerEvent::Update)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "UPDATE"));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                engine,
                &mut plan.view_rule_relations,
                &layer,
                uqa_sql::ast::RuleEvent::Update,
            )?
        };
        let layer_suppresses = has_view_rules
            && crate::sql::rules::relation_suppresses_original_query(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
            )?;
        if has_view_rules
            && crate::sql::rules::relation_has_returning_provider(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
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
                    .map(|assignment| assignment.column.clone())
                    .collect(),
                input_columns: Vec::new(),
            });
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_update_expressions(engine, &plan, &layer, source_schema.as_ref(), params)?;
        }
        let ordinary_subquery_ids = update_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
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
                engine,
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
        for (position, AssignmentPlan { column, value }) in plan.assignments.iter_mut().enumerate()
        {
            rewrite_target_expression(engine, value, &layer, ordinary_scope, &mut plan.subqueries)?;
            if layer_suppresses {
                *column = format!("\0uqa_view_rule_update_{position}");
            } else if !rewrite_suppressed {
                *column = writable_column(&layer, column, "UPDATE")?;
            }
        }
        let mapped = plan
            .assignments
            .iter()
            .map(|assignment| assignment.column.clone())
            .collect::<Vec<_>>();
        validate_mapped_columns(&mapped, duplicate_assignment)?;
        if let Some(predicate) = &mut plan.predicate {
            rewrite_target_expression(
                engine,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        rewrite_existing_view_checks(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, boundaries) = rewrite_returning(
            engine,
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
            engine,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        add_check_option(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        rewrite_suppressed |= layer_suppresses;
        if !super::super::view_triggers::target_is_view(engine, &plan.table)? {
            break;
        }
    }
    let input_columns = plan
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    for update_plan in &mut plan.view_rule_update_plans {
        update_plan.input_columns.clone_from(&input_columns);
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(engine, &plan.table)?;
        for assignment in &mut plan.assignments {
            bind_unqualified_source_positions(&mut assignment.value, source, target_width);
        }
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        engine,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    Ok(plan)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub(in crate::sql::dml) fn rewrite_delete_to_base(
    engine: &Engine,
    statement: &DeletePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<DeletePlan, SQLError> {
    let source_schema = dml_source_schema(
        engine,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
    )?;
    validate_public_delete_contract(engine, statement, source_schema.as_ref())?;
    validate_direct_view_rule_path(
        engine,
        &statement.table,
        uqa_sql::ast::RuleEvent::Delete,
        "DELETE",
    )?;
    let mut plan = statement.clone();
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    loop {
        let Some(layer) = automatic_view_layer(engine, &plan.table)? else {
            if active_unconditional_instead_rule(
                engine,
                &plan.table,
                uqa_sql::ast::RuleEvent::Delete,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "DELETE"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        validate_direct_view_rule_path(
            engine,
            &layer.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
            "DELETE",
        )?;
        if visited.len() > 1
            && instead_of_trigger_definition(engine, &layer.canonical_name, TriggerEvent::Delete)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "DELETE"));
        }
        if record_view_rule_relation(
            engine,
            &mut plan.view_rule_relations,
            &layer,
            uqa_sql::ast::RuleEvent::Delete,
        )? && crate::sql::rules::relation_has_returning_provider(
            engine,
            &layer.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
        )? {
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
            validate_delete_expressions(engine, &plan, &layer, source_schema.as_ref(), params)?;
        }
        let ordinary_subquery_ids = delete_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
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
                engine,
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
                engine,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        let (returning, boundaries) = rewrite_returning(
            engine,
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
            engine,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        if !super::super::view_triggers::target_is_view(engine, &plan.table)? {
            break;
        }
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(engine, &plan.table)?;
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        engine,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    Ok(plan)
}
