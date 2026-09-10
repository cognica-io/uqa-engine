//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build_join_spill_with_ctes, build_returning_value_row, coerce_view_value, dml_join_rows,
    eval_mutation_expr, finish_view_dml, materialize_view_rows, required_view_delete_columns,
    required_view_update_columns, resolve_view_target, target_columns, target_row,
    validate_dml_expression_qualifiers, validate_returning_alias_relations, view_document,
    view_qualification_references_target, with_statement_snapshot, BTreeSet, CteScope, DeletePlan,
    DmlReturningShape, OwnedPhysicalRow, ReturningValueProjectionRow, SQLError, SQLParam,
    SQLResult, ScalarExpr, SourceOutputPruning, StatementContext, UpdatePlan, Value, ViewDmlTarget,
};
use crate::mutation::{
    assignment::MutationAssignmentContext, rows::context::MutationExpressionContext,
};

mod delete;
pub use delete::run_view_delete_inner;

enum ViewDmlSourceMatch {
    TargetOnly,
    Source(OwnedPhysicalRow),
}

struct PendingViewUpdate {
    old: Vec<Value>,
    new: Vec<Value>,
    source_context: Option<OwnedPhysicalRow>,
    evaluation_row: OwnedPhysicalRow,
    evaluated_assignments: BTreeSet<String>,
}

fn evaluate_view_update_assignments<S: Clone + Send + Sync + 'static>(
    services: &MutationAssignmentContext<'_, S>,
    target: &ViewDmlTarget,
    stmt: &UpdatePlan,
    required: Option<&BTreeSet<String>>,
    pending: &mut PendingViewUpdate,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<(), SQLError> {
    for assignment in &stmt.assignments {
        if pending.evaluated_assignments.contains(&assignment.column)
            || required.is_some_and(|required| !required.contains(&assignment.column))
        {
            continue;
        }
        let position = target
            .columns
            .iter()
            .position(|column| column == &assignment.column)
            .ok_or_else(|| SQLError::UnknownColumn(assignment.column.clone()))?;
        let value = if matches!(assignment.value, ScalarExpr::Default) {
            Value::Null
        } else {
            eval_mutation_expr(
                services.expressions,
                scope,
                &assignment.value,
                Some(&pending.evaluation_row),
                params,
            )?
        };
        pending.new[position] = coerce_view_value(services.assignment, target, position, value)?;
        pending
            .evaluated_assignments
            .insert(assignment.column.clone());
    }
    Ok(())
}

fn matching_source_context<S: Clone + Send + Sync + 'static>(
    expressions: MutationExpressionContext<'_, S>,
    target_row: &OwnedPhysicalRow,
    source_rows: Option<&crate::SharedSpill>,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<ViewDmlSourceMatch>, SQLError> {
    let Some(source_rows) = source_rows else {
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(expressions, ctes, predicate, Some(target_row), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        return Ok(qualifies.then_some(ViewDmlSourceMatch::TargetOnly));
    };
    for source in source_rows
        .read_rows()
        .map_err(crate::physical::physical_exec_error)?
    {
        let source = source.map_err(crate::physical::physical_exec_error)?;
        let joined = dml_join_rows(target_row, &source);
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(expressions, ctes, predicate, Some(&joined), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        if qualifies {
            return Ok(Some(ViewDmlSourceMatch::Source(source)));
        }
    }
    Ok(None)
}

struct ViewSourceQualification<'a, S: Clone + 'static> {
    expressions: MutationExpressionContext<'a, S>,
    target: &'a ViewDmlTarget,
    target_qualifier: &'a str,
    predicate: Option<&'a ScalarExpr>,
    candidates: &'a [Vec<Value>],
    source_rows: &'a crate::SharedSpill,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
}

fn count_view_source_qualifications<S: Clone + Send + Sync + 'static>(
    context: ViewSourceQualification<'_, S>,
) -> Result<usize, SQLError> {
    let ViewSourceQualification {
        expressions,
        target,
        target_qualifier,
        predicate,
        candidates,
        source_rows,
        params,
        ctes,
    } = context;
    let references_target =
        view_qualification_references_target(target, target_qualifier, predicate);
    let mut count = 0;
    if !references_target {
        for source in source_rows
            .read_rows()
            .map_err(crate::physical::physical_exec_error)?
        {
            let source = source.map_err(crate::physical::physical_exec_error)?;
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(expressions, ctes, predicate, Some(&source), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            count += usize::from(qualifies);
        }
        return Ok(count);
    }
    for candidate in candidates {
        let physical = target_row(target, target_qualifier, candidate)?;
        for source in source_rows
            .read_rows()
            .map_err(crate::physical::physical_exec_error)?
        {
            let source = source.map_err(crate::physical::physical_exec_error)?;
            let joined = dml_join_rows(&physical, &source);
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(expressions, ctes, predicate, Some(&joined), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            count += usize::from(qualifies);
        }
    }
    Ok(count)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_update_inner<S: Clone + Send + Sync + 'static>(
    context: &StatementContext<'_, S>,
    prune_source_outputs: SourceOutputPruning,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(context.mutation.rules.views.rewrite, &stmt.table)?;
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    let _ = target_columns(&target, &assigned_columns, "UPDATE")?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
        for assignment in &stmt.assignments {
            validate_dml_expression_qualifiers(&assignment.value, &allowed)?;
        }
    }
    let original_query_survives =
        !uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.mutation.rules.rules.analysis,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
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
                uqa_sql::ast::TriggerEvent::Update,
                false,
                &assigned_columns,
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
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
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
        let row_independent_update_qualification = if stmt.source.is_none()
            && !context
                .mutation
                .rules
                .rules
                .analysis
                .rules
                .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Update)?
                .is_empty()
        {
            crate::mutation::expressions::row_independent_mutation_qualification_count(
                read_context
                    .mutation
                    .preparation
                    .referential
                    .assignment
                    .expressions,
                stmt.predicate.as_ref(),
                params,
                &ctes,
            )?
        } else {
            None
        };
        if stmt.view_rule_relations.is_empty()
            && !original_query_survives
            && !uqa_sql::semantics::rules::analysis::relation_rules_require_event_rows(
                context.mutation.rules.rules.analysis,
                &target.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
            )?
        {
            validate_returning_alias_relations(
                &stmt.target_qualifier,
                &stmt.returning_aliases,
                None,
            )?;
            let rule_batch = crate::mutation::rules::prepare_rule_batch(
                context.mutation.rules.rules,
                &target.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
                Vec::new(),
            )?;
            let outcome = rule_batch.execute_actions_with_affected(
                context.mutation.rules.rules,
                crate::mutation::rules::RuleReturningRequest::from_plan(
                    &stmt.returning,
                    &stmt.returning_aliases,
                    &stmt.subqueries,
                ),
            )?;
            if let Some(returning) = outcome.returning {
                return returning.project(
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
            return finish_view_dml(
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
                Vec::new(),
                0,
            );
        }
        let mut source_scope = ctes.returning_statement_snapshot_scope();
        let source_privilege_expressions = stmt
            .assignments
            .iter()
            .map(|assignment| &assignment.value)
            .chain(stmt.predicate.iter())
            .chain(stmt.returning.iter().map(|projection| &projection.expr))
            .collect::<Vec<_>>();
        if let Some(source) = stmt.source.as_deref() {
            crate::query::privileges::ensure_select_privileges_for_source_expressions(
                source,
                &source_privilege_expressions,
                &source_scope,
            )?;
        }
        let source_rows = stmt
            .source
            .as_deref()
            .map(|source| {
                build_join_spill_with_ctes(&read_context.source, source, params, &mut source_scope)
            })
            .transpose()?;
        validate_returning_alias_relations(
            &stmt.target_qualifier,
            &stmt.returning_aliases,
            source_rows.as_ref().map(crate::SharedSpill::row_schema),
        )?;
        let mut target_scope = ctes.returning_statement_snapshot_scope();
        let required_columns = (!original_query_survives)
            .then(|| {
                required_view_update_columns(context.mutation.rules.rules.analysis, &target, stmt)
            })
            .transpose()?
            .flatten();
        let candidates = materialize_view_rows(
            read_context,
            prune_source_outputs,
            &target,
            required_columns.as_ref(),
            params,
            &mut target_scope,
        )?;
        let snapshot = ctes.returning_statement_snapshot_scope();
        let source_update_qualification_count = source_rows
            .as_ref()
            .map(|source_rows| {
                count_view_source_qualifications(ViewSourceQualification {
                    expressions: read_context
                        .mutation
                        .preparation
                        .referential
                        .assignment
                        .expressions,
                    target: &target,
                    target_qualifier: &stmt.target_qualifier,
                    predicate: stmt.predicate.as_ref(),
                    candidates: &candidates,
                    source_rows,
                    params,
                    ctes: &snapshot,
                })
            })
            .transpose()?;
        let condition_columns = if !original_query_survives && stmt.view_rule_relations.is_empty() {
            Some(
                uqa_sql::semantics::rules::analysis::relation_condition_row_columns(
                    context.mutation.rules.rules.analysis,
                    &target.canonical_name,
                    uqa_sql::ast::RuleEvent::Update,
                )?,
            )
        } else {
            None
        };
        let mut pending = Vec::new();
        for old in candidates {
            let physical = target_row(&target, &stmt.target_qualifier, &old)?;
            let Some(source_match) = matching_source_context(
                read_context
                    .mutation
                    .preparation
                    .referential
                    .assignment
                    .expressions,
                &physical,
                source_rows.as_ref(),
                stmt.predicate.as_ref(),
                params,
                &snapshot,
            )?
            else {
                continue;
            };
            let source_context = match source_match {
                ViewDmlSourceMatch::TargetOnly => None,
                ViewDmlSourceMatch::Source(source) => Some(source),
            };
            let evaluation_row = source_context.as_ref().map_or_else(
                || physical.clone(),
                |source| dml_join_rows(&physical, source),
            );
            let mut row = PendingViewUpdate {
                new: old.clone(),
                old,
                source_context,
                evaluation_row,
                evaluated_assignments: BTreeSet::new(),
            };
            evaluate_view_update_assignments(
                &read_context.mutation.preparation.referential.assignment,
                &target,
                stmt,
                condition_columns.as_ref(),
                &mut row,
                params,
                &snapshot,
            )?;
            pending.push(row);
        }
        let rule_rows = pending
            .iter()
            .map(|row| {
                Ok(crate::mutation::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: Some(view_document(&target, &row.old)?),
                    new_storage_table: None,
                    new_doc_id: None,
                    new: Some(view_document(&target, &row.new)?),
                    context: row.source_context.clone(),
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut rule_batch = crate::mutation::rules::prepare_rule_batch(
            context.mutation.rules.rules,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
            rule_rows,
        )?;
        let update_qualification_count = source_update_qualification_count
            .or(row_independent_update_qualification)
            .unwrap_or_else(|| rule_batch.event_row_count());
        rule_batch.set_action_qualification_count(update_qualification_count);
        if !original_query_survives {
            let action_columns = rule_batch.matched_action_row_columns();
            for (row, required) in pending.iter_mut().zip(&action_columns) {
                evaluate_view_update_assignments(
                    &read_context.mutation.preparation.referential.assignment,
                    &target,
                    stmt,
                    Some(required),
                    row,
                    params,
                    &snapshot,
                )?;
            }
            rule_batch.supplement_rows(
                pending
                    .iter()
                    .map(|row| {
                        Ok(crate::mutation::rules::RuleRowImage {
                            old_storage_table: None,
                            old_doc_id: None,
                            old: Some(view_document(&target, &row.old)?),
                            new_storage_table: None,
                            new_doc_id: None,
                            new: Some(view_document(&target, &row.new)?),
                            context: row.source_context.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?,
            )?;
            let outer_rule_rows = pending
                .iter()
                .map(|row| {
                    Ok(crate::mutation::rules::RuleRowImage {
                        old_storage_table: None,
                        old_doc_id: None,
                        old: Some(view_document(&target, &row.old)?),
                        new_storage_table: None,
                        new_doc_id: None,
                        new: Some(view_document(&target, &row.new)?),
                        context: row.source_context.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            let mut outer_rule_batches = crate::mutation::rules::views::prepare_view_rule_batches(
                crate::mutation::rules::views::ViewRuleBatchRequest {
                    context: context.mutation.rules,
                    relations: &stmt.view_rule_relations,
                    event: uqa_sql::ast::RuleEvent::Update,
                    rows: &outer_rule_rows,
                    params,
                    scope: &snapshot,
                    insert_plans: &[],
                    update_plans: &stmt.view_rule_update_plans,
                    document_relation: Some(&target.canonical_name),
                },
            )?;
            outer_rule_batches.configure_action_qualification(Some(update_qualification_count));
            let outer_outcome = outer_rule_batches.execute_actions_with_affected(
                context.mutation.rules.rules,
                stmt.view_rule_returning.as_ref(),
            )?;
            let outcome = rule_batch.execute_actions_with_affected(
                context.mutation.rules.rules,
                crate::mutation::rules::RuleReturningRequest::from_plan(
                    &stmt.returning,
                    &stmt.returning_aliases,
                    &stmt.subqueries,
                ),
            )?;
            let affected = if outer_outcome.sets_command_tag {
                outer_outcome.affected_rows
            } else {
                outcome.affected_rows
            };
            if outcome.returning.is_some() && outer_outcome.returning.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot have RETURNING lists in multiple rules".into(),
                });
            }
            if let Some(returning) = outcome.returning {
                return returning.project(
                    context.mutation.preparation.returning,
                    DmlReturningShape {
                        table: &target.canonical_name,
                        target_qualifier: &stmt.target_qualifier,
                        aliases: &stmt.returning_aliases,
                        returning: &stmt.returning,
                        params,
                        ctes: &ctes,
                        supplemental_schema: source_rows
                            .as_ref()
                            .map(crate::SharedSpill::row_schema),
                    },
                );
            }
            if let Some(returning) = outer_outcome.returning {
                return returning.project(
                    context.mutation.preparation.returning,
                    params,
                    &ctes,
                    source_rows.as_ref().map(crate::SharedSpill::row_schema),
                );
            }
            return finish_view_dml(
                &context.mutation.preparation.returning,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: source_rows.as_ref().map(crate::SharedSpill::row_schema),
                },
                Vec::new(),
                affected,
            );
        }
        let rule_returning = rule_batch.execute_actions(
            context.mutation.rules.rules,
            crate::mutation::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        let mut affected = 0_u64;
        let mut returning_rows = Vec::new();
        for (index, row) in pending.into_iter().enumerate() {
            if rule_batch.suppresses(index) {
                continue;
            }
            let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
                &context.mutation.preparation.referential.triggers,
                &target.canonical_name,
                uqa_sql::ast::TriggerEvent::Update,
                Some(&row.old),
                Some(&row.new),
                &assigned_columns,
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
                        old: Some(&row.old),
                        new: Some(&final_new),
                        aliases: &stmt.returning_aliases,
                        context: row.source_context.as_ref(),
                    },
                    &stmt.returning,
                    params,
                    &ctes,
                )?);
            }
        }
        crate::mutation::triggers::fire_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
        let result = finish_view_dml(
            &context.mutation.preparation.returning,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows.as_ref().map(crate::SharedSpill::row_schema),
            },
            returning_rows,
            affected,
        )?;
        if let Some(rule_returning) = rule_returning {
            return rule_returning.project(
                context.mutation.preparation.returning,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: source_rows.as_ref().map(crate::SharedSpill::row_schema),
                },
            );
        }
        Ok(result)
    };
    match statement_snapshot.as_deref() {
        Some(snapshot) => with_statement_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
}
