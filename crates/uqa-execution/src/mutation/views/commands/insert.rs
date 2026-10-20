//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution for views with `INSTEAD OF` triggers.

use super::{
    eval_mutation_expr, resolve_view_target, run_suppressed_view_insert_rules, target_columns,
    validate_returning_alias_relations, values_from_result, view_document, with_mutation_snapshot,
    BTreeSet, CteScope, DmlReturningShape, InsertPlan, MutationStatementContext, SQLError,
    SQLParam, ScalarExpr, SourceOutputPruning, Value,
};
use crate::mutation::output::MutationOutput;
use crate::query::statement::consumer::QueryOutputMode;
use std::{cell::Cell, rc::Rc};

mod rows;
use rows::{ViewInsertRows, ViewInsertSource};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_insert_inner<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune_source_outputs: SourceOutputPruning,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
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
                .position(|candidate| candidate == &column.column)
                .ok_or_else(|| SQLError::UnknownColumn(column.column.clone()))
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
    let (statement_commands, _running_statement) =
        crate::mutation::statement_end::statement_commands(inherited_ctes);
    let view_statement = crate::mutation::triggers::queue::StatementEvent::new(
        &target.canonical_name,
        uqa_sql::ast::TriggerEvent::Insert,
        &[],
    );
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
        statement_commands.after_triggers().fire_before_statement(
            &context.mutation.preparation.referential.triggers,
            &view_statement,
        )?;
    }
    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<MutationOutput, SQLError> {
            let ctes = statement_scope.insert(read_context.mutation.scopes.command_scope(
                stmt.statement_privilege_subject.as_ref(),
                stmt.relations_bound,
            )?);
            if let Some(parent) = inherited_ctes {
                ctes.inherit_cte_bindings(parent);
            }
            ctes.set_statement_commands(std::sync::Arc::clone(&statement_commands));
            ctes.set_command_cte_snapshot(statement_snapshot.clone());
            crate::query::cte::materialize_command_ctes(
                context.query.source.ctes,
                &stmt.ctes,
                || {
                    uqa_sql::semantics::primary_command_cte_references(
                        &stmt.ctes,
                        &stmt.query_inputs(),
                        None,
                    )
                },
                params,
                ctes,
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
                    context.mutation.rules.rules,
                    &context.mutation.preparation.returning,
                    &read_context.mutation.preparation.referential.assignment,
                    stmt,
                    &target,
                    &positions,
                    &columns,
                    implicit_columns,
                    params,
                    ctes,
                );
            }
            let row_output = Rc::new(ViewInsertRows::new(
                context,
                read_context,
                &target,
                stmt,
                params,
                ctes,
            )?);
            let mut has_rules = false;
            for relation in std::iter::once(&target.canonical_name).chain(&stmt.view_rule_relations)
            {
                if !context
                    .mutation
                    .rules
                    .rules
                    .analysis
                    .rules
                    .rules_for(relation, uqa_sql::ast::RuleEvent::Insert)?
                    .is_empty()
                {
                    has_rules = true;
                    break;
                }
            }
            if !has_rules {
                if let Some(source) = stmt.source.as_deref() {
                    let mut source_scope = ctes.returning_statement_snapshot_scope();
                    source_scope.enable_command_progress_streaming();
                    crate::query::statement::execute_query_plan_output(
                        &read_context.query,
                        source,
                        params,
                        &mut source_scope,
                        QueryOutputMode::physical_consumer(Rc::new(ViewInsertSource {
                            rows: Rc::clone(&row_output),
                            columns: &columns,
                            positions: &positions,
                            width: Cell::new(0),
                        })),
                    )?;
                } else {
                    row_output.values(&columns, &positions, &stmt.rows)?;
                }
                crate::mutation::statement_end::end_command(
                    &statement_commands,
                    &context.mutation.preparation.referential.triggers,
                    std::slice::from_ref(&view_statement),
                    Vec::new(),
                )?;
                return row_output.finish();
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
                                required_columns
                                    .contains(&column.column)
                                    .then_some(position)
                            })
                            .collect::<BTreeSet<_>>();
                        prune_source_outputs(&mut source, &required_positions, columns.len());
                    }
                }
                let mut source_scope = ctes.returning_statement_snapshot_scope();
                let result = crate::query::statement::execute_query_plan_with_ctes(
                    &read_context.query,
                    &source,
                    params,
                    &mut source_scope,
                )?;
                let types = result.column_types.clone();
                values_from_result(result)?
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .enumerate()
                            .map(|(position, value)| {
                                (value, types.get(position).cloned().flatten())
                            })
                            .collect()
                    })
                    .collect()
            } else {
                let snapshot = ctes.returning_statement_snapshot_scope();
                let expressions = read_context
                    .mutation
                    .preparation
                    .referential
                    .assignment
                    .expressions;
                let hook = expressions.expressions.bind_scope(snapshot.clone());
                let schema = crate::RowSchema::default();
                stmt.rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|expression| {
                                if matches!(expression, ScalarExpr::Default) {
                                    Ok((Value::Null, None))
                                } else {
                                    let source = uqa_sql::type_resolution::assignment_source_type(
                                        expression,
                                        &schema,
                                        params,
                                        hook.as_ref(),
                                    )?;
                                    let value = eval_mutation_expr(
                                        expressions,
                                        &snapshot,
                                        expression,
                                        None,
                                        params,
                                    )?;
                                    Ok((value, source))
                                }
                            })
                            .collect()
                    })
                    .collect::<Result<Vec<Vec<_>>, SQLError>>()?
            };
            let mut proposed_rows = Vec::with_capacity(input_rows.len());
            for input in input_rows {
                let width = input.len();
                proposed_rows.push(row_output.project(
                    &columns,
                    &positions,
                    width,
                    input.into_iter().map(Ok),
                )?);
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
                    scope: ctes,
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
            for (index, new) in proposed_rows.into_iter().enumerate() {
                if !rule_batch.suppresses(index) {
                    row_output.apply(&new)?;
                }
            }
            if original_query_survives {
                crate::mutation::statement_end::end_command(
                    &statement_commands,
                    &context.mutation.preparation.referential.triggers,
                    std::slice::from_ref(&view_statement),
                    Vec::new(),
                )?;
            }
            let mut result = row_output.finish()?;
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
                        ctes,
                        supplemental_schema: None,
                    },
                );
            }
            if let Some(outer_returning) = outer_rule_outcome.returning {
                return outer_returning.project(
                    context.mutation.preparation.returning,
                    params,
                    ctes,
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
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }?;
    crate::mutation::statement_end::finish_statement(
        context,
        params,
        &stmt.ctes,
        statement_scope.as_mut(),
    )?;
    Ok(result)
}
