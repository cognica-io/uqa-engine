//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    active_unconditional_instead_rule, add_check_option, automatic_view_layer,
    duplicate_assignment, insert_conflict_subquery_ids, insert_input_width,
    instead_of_trigger_definition, not_automatically_updatable, preserve_view_rule_returning,
    record_view_rule_relation, returning_subquery_ids, rewrite_correlated_dml_context,
    rewrite_existing_view_checks, rewrite_returning, rewrite_target_expression,
    validate_direct_view_rule_path, validate_insert_expressions, validate_insert_targets,
    validate_mapped_columns, validate_public_insert_contract, validate_public_view_targets,
    view_updatability, writable_column, BTreeSet, ConflictActionPlan, CorrelatedDmlContext,
    ExpressionScope, InsertPlan, SQLError, TriggerEvent, ViewRewriteContext, ViewRuleInsertPlan,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn rewrite_insert_to_base(
    services: ViewRewriteContext<'_>,
    statement: &InsertPlan,
    params: &[crate::SQLParam],
    inherited_ctes: Option<&super::CteScope>,
) -> Result<InsertPlan, SQLError> {
    validate_public_view_targets(
        services,
        &statement.table,
        statement.columns.iter().map(String::as_str),
    )?;
    validate_public_insert_contract(services, statement)?;
    let Some(initial_layer) = automatic_view_layer(services, &statement.table)? else {
        return Err(not_automatically_updatable(&statement.table, "INSERT"));
    };
    validate_insert_targets(&initial_layer, statement)?;
    validate_direct_view_rule_path(
        services,
        &initial_layer.canonical_name,
        crate::ast::RuleEvent::Insert,
        "INSERT",
    )?;
    if !view_updatability(services, &statement.table)?
        .automatic
        .insertable
    {
        return Err(not_automatically_updatable(&statement.table, "INSERT"));
    }
    let mut plan = statement.clone();
    let next_privilege_subject =
        crate::semantics::view_privileges::ensure_insert(services.authorization, &plan)?;
    plan.target_privilege_subject = Some(next_privilege_subject);
    let mut implicit_width = if statement.columns.is_empty() {
        Some(insert_input_width(
            services,
            statement,
            params,
            inherited_ctes,
        )?)
    } else {
        None
    };
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut rewrite_suppressed = false;
    loop {
        let Some(layer) = automatic_view_layer(services, &plan.table)? else {
            if active_unconditional_instead_rule(
                services,
                &plan.table,
                crate::ast::RuleEvent::Insert,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "INSERT"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        if !rewrite_suppressed {
            validate_direct_view_rule_path(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Insert,
                "INSERT",
            )?;
        }
        if !rewrite_suppressed
            && visited.len() > 1
            && instead_of_trigger_definition(services, &layer.canonical_name, TriggerEvent::Insert)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "INSERT"));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                services,
                &mut plan.view_rule_relations,
                &layer,
                crate::ast::RuleEvent::Insert,
            )?
        };
        let layer_suppresses = has_view_rules
            && super::context::relation_suppresses_original_query(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Insert,
            )?;
        if visited.len() > 1 && !rewrite_suppressed && !layer_suppresses {
            let next_privilege_subject =
                crate::semantics::view_privileges::ensure_insert(services.authorization, &plan)?;
            plan.target_privilege_subject = Some(next_privilege_subject);
        }
        if has_view_rules
            && super::context::relation_has_returning_provider(
                services,
                &layer.canonical_name,
                crate::ast::RuleEvent::Insert,
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
            validate_insert_expressions(services, &plan, &layer, params, inherited_ctes)?;
        }
        let conflict_subquery_ids = insert_conflict_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                inherited_ctes,
                services,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: None,
                include_excluded: true,
                ctes: &plan.ctes,
                ids: &conflict_subquery_ids,
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
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let supplied_columns = if let Some(width) = implicit_width.take() {
            layer
                .columns
                .iter()
                .take(width)
                .map(|column| column.name.clone())
                .collect::<Vec<_>>()
        } else {
            plan.columns.clone()
        };
        let columns = if rewrite_suppressed || layer_suppresses {
            supplied_columns.clone()
        } else {
            supplied_columns
                .iter()
                .map(|column| writable_column(&layer, column, "INSERT"))
                .collect::<Result<Vec<_>, _>>()?
        };
        if has_view_rules {
            plan.view_rule_insert_plans.push(ViewRuleInsertPlan {
                relation: layer.canonical_name.clone(),
                supplied_columns,
                input_columns: Vec::new(),
            });
        }
        validate_mapped_columns(&columns, duplicate_assignment)?;
        if let Some(conflict) = &mut plan.on_conflict {
            for predicate in conflict
                .expressions
                .iter_mut()
                .chain(conflict.predicate.iter_mut().map(Box::as_mut))
            {
                rewrite_target_expression(
                    services,
                    predicate,
                    &layer,
                    ExpressionScope {
                        target_qualifier: &target_qualifier,
                        returning_aliases: None,
                        source: None,
                        include_excluded: false,
                    },
                    &mut plan.subqueries,
                )?;
            }
            conflict.conflict_columns = conflict
                .conflict_columns
                .iter()
                .map(|column| writable_column(&layer, column, "INSERT"))
                .collect::<Result<Vec<_>, _>>()?;
            if let ConflictActionPlan::Update {
                assignments,
                predicate,
            } = &mut conflict.action
            {
                let scope = ExpressionScope {
                    target_qualifier: &target_qualifier,
                    returning_aliases: None,
                    source: None,
                    include_excluded: true,
                };
                for assignment in assignments.iter_mut() {
                    assignment.column = writable_column(&layer, &assignment.column, "UPDATE")?;
                    rewrite_target_expression(
                        services,
                        &mut assignment.value,
                        &layer,
                        scope,
                        &mut plan.subqueries,
                    )?;
                }
                let mapped = assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
                validate_mapped_columns(&mapped, duplicate_assignment)?;
                if let Some(predicate) = predicate {
                    rewrite_target_expression(
                        services,
                        predicate,
                        &layer,
                        scope,
                        &mut plan.subqueries,
                    )?;
                }
            }
        }
        rewrite_existing_view_checks(
            services,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, _) = rewrite_returning(
            services,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            None,
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        add_check_option(
            services,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.columns = columns;
        plan.table = layer.source_name;
        plan.include_descendants = true;
        rewrite_suppressed |= layer_suppresses;
        if !super::context::target_is_view(services, &plan.table)? {
            break;
        }
    }
    for insert_plan in &mut plan.view_rule_insert_plans {
        insert_plan.input_columns.clone_from(&plan.columns);
    }
    Ok(plan)
}
