//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    add_check_option, canonical_view_name, duplicate_assignment, insert_conflict_subquery_ids,
    insert_input_width, instead_of_trigger_definition, next_rewritten_layer,
    preserve_view_rule_returning, record_view_rule_relation, returning_subquery_ids,
    rewritable_layer, rewrite_correlated_dml_context, rewrite_existing_view_checks,
    rewrite_returning, rewrite_target_expression, validate_direct_view_rule_path,
    validate_insert_expressions, validate_insert_targets, validate_mapped_columns,
    validate_public_insert_contract, validate_public_view_targets, validate_writable_columns,
    view_not_updatable, writable_column, BTreeSet, ColumnWrite, ConflictActionPlan,
    CorrelatedDmlContext, ExpressionScope, InsertPlan, LayerPrivileges, NotUpdatableReason,
    SQLError, TriggerEvent, ViewCommand, ViewRewriteContext, ViewRuleInsertPlan,
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
        statement
            .columns
            .iter()
            .map(|target| target.column.as_str()),
    )?;
    validate_public_insert_contract(services, statement)?;
    let view = canonical_view_name(services, &statement.table)?;
    validate_direct_view_rule_path(
        services,
        &view,
        crate::ast::RuleEvent::Insert,
        ViewCommand::Insert,
    )?;
    let initial_layer = rewritable_layer(services, &view, ViewCommand::Insert)?;
    if !initial_layer.has_writable_column() {
        return Err(view_not_updatable(
            &view,
            ViewCommand::Insert,
            NotUpdatableReason::NoUpdatableColumns,
        ));
    }
    validate_insert_targets(&initial_layer, statement)?;
    let mut initial_layer = Some(initial_layer);
    let mut plan = statement.clone();
    let mut privileges = LayerPrivileges::new();
    plan.target_privilege_subject = Some(privileges.check(
        services.authorization,
        &plan.table,
        plan.target_privilege_subject.as_ref(),
        || crate::semantics::view_privileges::ensure_insert(services.authorization, &plan),
    )?);
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
        // An underlying view with an INSTEAD OF trigger ends the rewrite, since `RewriteQuery` rewrites only a view without one: the trigger performs the INSERT on that view.
        if !visited.is_empty()
            && !rewrite_suppressed
            && instead_of_trigger_definition(services, &plan.table, TriggerEvent::Insert)?
        {
            break;
        }
        let Some(layer) = next_rewritten_layer(
            services,
            &plan.table,
            &mut initial_layer,
            rewrite_suppressed,
            crate::ast::RuleEvent::Insert,
            ViewCommand::Insert,
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
            if !layer.has_writable_column() {
                return Err(view_not_updatable(
                    &layer.canonical_name,
                    ViewCommand::Insert,
                    NotUpdatableReason::NoUpdatableColumns,
                ));
            }
            plan.target_privilege_subject = Some(privileges.check(
                services.authorization,
                &plan.table,
                plan.target_privilege_subject.as_ref(),
                || crate::semantics::view_privileges::ensure_insert(services.authorization, &plan),
            )?);
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
                .map(|column| column.name.clone().into())
                .collect::<Vec<_>>()
        } else {
            plan.columns.clone()
        };
        let columns = if rewrite_suppressed || layer_suppresses {
            supplied_columns.clone()
        } else {
            let conflict_updates = match plan.on_conflict.as_ref().map(|conflict| &conflict.action)
            {
                Some(ConflictActionPlan::Update { assignments, .. }) => assignments.as_slice(),
                _ => &[],
            };
            validate_writable_columns(
                &layer,
                supplied_columns
                    .iter()
                    .map(|target| target.column.as_str())
                    .chain(
                        conflict_updates
                            .iter()
                            .map(|assignment| assignment.target.column.as_str()),
                    ),
                ColumnWrite::Insert,
            )?;
            supplied_columns
                .clone()
                .into_iter()
                .map(|mut target| {
                    target.column = writable_column(&layer, &target.column, ColumnWrite::Insert)?;
                    Ok::<_, SQLError>(target)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        if has_view_rules {
            plan.view_rule_insert_plans.push(ViewRuleInsertPlan {
                relation: layer.canonical_name.clone(),
                supplied_columns: supplied_columns
                    .into_iter()
                    .map(|target| target.column)
                    .collect(),
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
                .map(|column| writable_column(&layer, column, ColumnWrite::Insert))
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
                    assignment.target.column =
                        writable_column(&layer, &assignment.target.column, ColumnWrite::Insert)?;
                    for expression in assignment.expressions_mut() {
                        rewrite_target_expression(
                            services,
                            expression,
                            &layer,
                            scope,
                            &mut plan.subqueries,
                        )?;
                    }
                }
                let mapped = assignments
                    .iter()
                    .map(|assignment| assignment.target.clone())
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
        insert_plan.input_columns = plan
            .columns
            .iter()
            .map(|target| target.column.clone())
            .collect();
    }
    privileges.finish()?;
    Ok(plan)
}
