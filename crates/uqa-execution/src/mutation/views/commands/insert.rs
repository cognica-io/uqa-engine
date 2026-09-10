//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution for views with `INSTEAD OF` triggers.

use super::{
    build_returning_value_row, coerce_view_value, eval_mutation_expr, finish_view_dml,
    resolve_view_target, run_suppressed_view_insert_rules, target_columns,
    validate_returning_alias_relations, values_from_result, view_document, with_statement_snapshot,
    BTreeSet, CteScope, DmlReturningShape, InsertPlan, ReturningValueProjectionRow, SQLError,
    SQLParam, SQLResult, ScalarExpr, SourceOutputPruning, StatementContext, Value,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_insert_inner<S: Clone + Send + Sync + 'static>(
    context: &StatementContext<'_, S>,
    prune_source_outputs: SourceOutputPruning,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(context.mutation.rules.views.rewrite, &stmt.table)?;
    if stmt.on_conflict.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "INSERT with ON CONFLICT clause cannot be used with a view".into(),
        });
    }
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    let columns = target_columns(&target, &stmt.columns, "INSERT")?;
    let implicit_columns = stmt.columns.is_empty();
    let positions = columns
        .iter()
        .map(|column| {
            target
                .columns
                .iter()
                .position(|candidate| candidate == column)
                .ok_or_else(|| SQLError::UnknownColumn(column.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let original_query_survives =
        !uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.mutation.rules.rules.analysis,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Insert,
        )?;
    let has_before_statement_trigger = original_query_survives
        && !context
            .mutation
            .preparation
            .referential
            .triggers
            .catalog
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
                false,
                &[],
            )?
            .is_empty();
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(context.snapshots.capture()?))
        }
        None => None,
    };
    if original_query_survives {
        crate::mutation::triggers::fire_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    let execute_read = |read_context: &StatementContext<'_, S>| -> Result<SQLResult, SQLError> {
        let mut ctes = read_context.mutation.scopes.command_scope(
            stmt.statement_privilege_subject.as_deref(),
            stmt.relations_bound,
        )?;
        if let Some(parent) = inherited_ctes {
            ctes.inherit_cte_bindings(parent);
        }
        ctes.set_command_cte_snapshot(statement_snapshot.clone());
        crate::query::cte::materialize_plan_ctes(
            context.source.ctes,
            &stmt.ctes,
            params,
            &mut ctes,
        )?;
        ctes.scalar_subqueries.clone_from(&stmt.subqueries);
        let suppressed_source_is_unused = stmt.source.is_some()
            && !uqa_sql::semantics::rules::analysis::relation_rules_require_event_rows(
                context.mutation.rules.rules.analysis,
                &target.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
            )?;
        if stmt.view_rule_relations.is_empty()
            && !original_query_survives
            && (stmt.source.is_none() || suppressed_source_is_unused)
        {
            return run_suppressed_view_insert_rules(
                context,
                &read_context.mutation.preparation.referential.assignment,
                stmt,
                &target,
                &positions,
                &columns,
                implicit_columns,
                params,
                &ctes,
            );
        }
        let input_rows = if let Some(source) = stmt.source.as_deref() {
            let mut source = source.clone();
            if !original_query_survives {
                if let Some(required_columns) =
                    uqa_sql::semantics::rules::analysis::relation_rule_row_columns(
                        context.mutation.rules.rules.analysis,
                        &target.canonical_name,
                        uqa_sql::ast::RuleEvent::Insert,
                    )?
                {
                    let required_positions = columns
                        .iter()
                        .enumerate()
                        .filter_map(|(position, column)| {
                            required_columns.contains(column).then_some(position)
                        })
                        .collect::<BTreeSet<_>>();
                    prune_source_outputs(&mut source, &required_positions, columns.len());
                }
            }
            let mut source_scope = ctes.returning_statement_snapshot_scope();
            values_from_result(crate::query::statement::execute_query_plan_with_ctes(
                read_context,
                &source,
                params,
                &mut source_scope,
            )?)?
        } else {
            let snapshot = ctes.returning_statement_snapshot_scope();
            stmt.rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|expression| {
                            if matches!(expression, ScalarExpr::Default) {
                                Ok(Value::Null)
                            } else {
                                eval_mutation_expr(
                                    read_context
                                        .mutation
                                        .preparation
                                        .referential
                                        .assignment
                                        .expressions,
                                    &snapshot,
                                    expression,
                                    None,
                                    params,
                                )
                            }
                        })
                        .collect()
                })
                .collect::<Result<Vec<Vec<_>>, SQLError>>()?
        };
        let mut proposed_rows = Vec::with_capacity(input_rows.len());
        for input in input_rows {
            if input.len() > columns.len() || (!implicit_columns && input.len() != columns.len()) {
                return Err(SQLError::TypeMismatch(format!(
                    "row width {} != column count {}",
                    input.len(),
                    columns.len()
                )));
            }
            let mut new = vec![Value::Null; target.columns.len()];
            for (input_position, target_position) in positions.iter().copied().enumerate() {
                if let Some(value) = input.get(input_position) {
                    new[target_position] = coerce_view_value(
                        context
                            .mutation
                            .preparation
                            .referential
                            .assignment
                            .assignment,
                        &target,
                        target_position,
                        value.clone(),
                    )?;
                }
            }
            proposed_rows.push(new);
        }
        let rule_rows = proposed_rows
            .iter()
            .map(|new| {
                Ok(crate::mutation::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: None,
                    new_storage_table: None,
                    new_doc_id: None,
                    new: Some(view_document(&target, new)?),
                    context: None,
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let outer_rule_batches = crate::mutation::rules::views::prepare_view_rule_batches(
            crate::mutation::rules::views::ViewRuleBatchRequest {
                context: context.mutation.rules,
                relations: &stmt.view_rule_relations,
                event: uqa_sql::ast::RuleEvent::Insert,
                rows: &rule_rows,
                params,
                scope: &ctes,
                insert_plans: &stmt.view_rule_insert_plans,
                update_plans: &[],
                document_relation: Some(&target.canonical_name),
            },
        )?;
        let rule_batch = crate::mutation::rules::prepare_rule_batch(
            context.mutation.rules.rules,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Insert,
            rule_rows,
        )?;
        let mut affected = 0_u64;
        let mut returning_rows = Vec::new();
        for (index, new) in proposed_rows.into_iter().enumerate() {
            if rule_batch.suppresses(index) {
                continue;
            }
            let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
                &context.mutation.preparation.referential.triggers,
                &target.canonical_name,
                uqa_sql::ast::TriggerEvent::Insert,
                None,
                Some(&new),
                &[],
            )?
            else {
                continue;
            };
            affected += 1;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_value_row(
                    context.mutation.preparation.returning,
                    ReturningValueProjectionRow {
                        table: &target.canonical_name,
                        target_qualifier: &stmt.target_qualifier,
                        current: &final_new,
                        old: None,
                        new: Some(&final_new),
                        aliases: &stmt.returning_aliases,
                        context: None,
                    },
                    &stmt.returning,
                    params,
                    &ctes,
                )?);
            }
        }
        if original_query_survives {
            crate::mutation::triggers::fire_statement_triggers(
                &context.mutation.preparation.referential.triggers,
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::After,
                uqa_sql::ast::TriggerEvent::Insert,
                &[],
            )?;
        }
        let mut result = finish_view_dml(
            &context.mutation.preparation.returning,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
            },
            returning_rows,
            affected,
        )?;
        let rule_outcome = rule_batch.execute_actions_with_affected(
            context.mutation.rules.rules,
            crate::mutation::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        let outer_rule_outcome = outer_rule_batches.execute_actions_with_affected(
            context.mutation.rules.rules,
            stmt.view_rule_returning.as_ref(),
        )?;
        if rule_outcome.returning.is_some() && outer_rule_outcome.returning.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot have RETURNING lists in multiple rules".into(),
            });
        }
        if let Some(rule_returning) = rule_outcome.returning {
            return rule_returning.project(
                context.mutation.preparation.returning,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: None,
                },
            );
        }
        if let Some(outer_returning) = outer_rule_outcome.returning {
            return outer_returning.project(
                context.mutation.preparation.returning,
                params,
                &ctes,
                None,
            );
        }
        if !original_query_survives && rule_outcome.sets_command_tag {
            result.affected_rows = rule_outcome.affected_rows;
        }
        if !original_query_survives && outer_rule_outcome.sets_command_tag {
            result.affected_rows = outer_rule_outcome.affected_rows;
        }
        Ok(result)
    };
    match statement_snapshot.as_deref() {
        Some(snapshot) => with_statement_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
}
