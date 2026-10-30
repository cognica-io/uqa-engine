//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build_join_spill_with_ctes, build_returning_value_row, dml_join_rows, eval_mutation_expr,
    finish_view_dml, materialize_view_rows, resolve_view_target, target_columns, target_row,
    validate_dml_expression_qualifiers, validate_returning_alias_relations, view_document,
    with_mutation_snapshot, BTreeSet, CteScope, DeletePlan, DmlReturningShape,
    MutationStatementContext, OwnedPhysicalRow, ReturningValueProjectionRow, SQLError, SQLParam,
    ScalarExpr, TriggerViewChecks, UpdatePlan, Value, ViewDmlTarget,
};
use crate::mutation::output::MutationOutput;
use crate::mutation::{
    assignment::MutationAssignmentContext, rows::context::MutationExpressionContext,
};

mod delete;
pub use delete::run_view_delete_inner;
mod pending;
use crate::mutation::row_images::MutationImageBuffer;
use pending::{view_values, PendingViewUpdate};

enum ViewDmlSourceMatch {
    TargetOnly,
    Source(OwnedPhysicalRow),
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
    for (assignment_index, assignment) in stmt.assignments.iter().enumerate() {
        if pending.evaluated_assignments.contains(&assignment_index)
            || required.is_some_and(|required| {
                assignment
                    .target
                    .targets()
                    .iter()
                    .all(|target| !required.contains(&target.column))
            })
        {
            continue;
        }
        let source = crate::mutation::assignment::AssignmentSource::evaluate(
            *services,
            scope,
            assignment,
            Some(&pending.evaluation_row),
            params,
        )?;
        for (target_position, assignment_target) in assignment.target.targets().iter().enumerate() {
            let position = target
                .columns
                .iter()
                .position(|column| column == &assignment_target.column)
                .ok_or_else(|| SQLError::UnknownColumn(assignment_target.column.clone()))?;
            pending.new[position] = crate::mutation::assignment::eval_typed_assignment_input(
                *services,
                scope,
                crate::mutation::assignment::TypedAssignmentTarget {
                    target: assignment_target,
                    ty: target.schema.column_type(position),
                    current: Some(&pending.new[position]),
                    final_column_write: crate::mutation::assignment::final_column_write(
                        &stmt.assignments,
                        assignment_index,
                        target_position,
                    ),
                },
                source.input(assignment.target.source_position(target_position)),
                Some(&pending.evaluation_row),
                params,
            )?;
        }
        pending.evaluated_assignments.insert(assignment_index);
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn run_view_update_inner<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let target = resolve_view_target(context, &stmt.table)?;
    let assigned_columns = stmt
        .assignments
        .iter()
        .flat_map(|assignment| assignment.target.column_names())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let _ = target_columns(
        &target,
        &stmt
            .assignments
            .iter()
            .flat_map(|assignment| assignment.target.targets().iter().cloned())
            .collect::<Vec<_>>(),
        "UPDATE",
    )?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
        for assignment in &stmt.assignments {
            for expression in assignment.expressions() {
                validate_dml_expression_qualifiers(expression, &allowed)?;
            }
        }
    }
    let has_before_statement_trigger = !context
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
    let (statement_commands, _running_statement) =
        crate::mutation::statement_end::statement_commands(inherited_ctes);
    let view_statement = crate::mutation::triggers::queue::StatementEvent::new(
        &target.canonical_name,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
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

    statement_commands.after_triggers().fire_before_statement(
        &context.mutation.preparation.referential.triggers,
        &view_statement,
    )?;

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
                |control| {
                    uqa_sql::semantics::primary_command_cte_references_with_control(
                        &stmt.ctes,
                        stmt.query_inputs(),
                        stmt.source_input(),
                        control,
                    )
                },
                params,
                ctes,
            )?;
            ctes.scalar_subqueries = stmt.subqueries.as_slice().into();
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
                    build_join_spill_with_ctes(
                        &read_context.query.source,
                        source,
                        params,
                        &mut source_scope,
                    )
                })
                .transpose()?;
            validate_returning_alias_relations(
                &stmt.target_qualifier,
                &stmt.returning_aliases,
                source_rows.as_ref().map(crate::SharedSpill::row_schema),
            )?;
            let mut target_scope = ctes.returning_statement_snapshot_scope();
            let candidates =
                materialize_view_rows(&read_context.query, &target, params, &mut target_scope)?;
            let snapshot = ctes.returning_statement_snapshot_scope();
            let work_mem = context.mutation.rules.rules.memory.work_mem_bytes()?;
            let cancellation = context.mutation.rules.rules.cancellation.clone();
            let mut pending = MutationImageBuffer::new(work_mem, cancellation.clone());
            for candidate in candidates
                .read_rows()
                .map_err(crate::physical::physical_exec_error)?
            {
                cancellation.check()?;
                let old = candidate
                    .map_err(crate::physical::physical_exec_error)?
                    .row
                    .into_physical_values();
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
                    None,
                    &mut row,
                    params,
                    &snapshot,
                )?;
                pending.push(row.into_image(&target)?)?;
            }
            let pending = pending.finish();
            drop(candidates);
            let checks = TriggerViewChecks {
                expressions: read_context
                    .mutation
                    .preparation
                    .referential
                    .assignment
                    .expressions,
                constraints: read_context.mutation.preparation.referential.constraints,
                target: &target,
                target_qualifier: &stmt.target_qualifier,
                checks: &stmt.view_checks,
                supplied: &assigned_columns,
                params,
                scope: ctes,
            };
            let mut affected = 0_u64;
            let mut returning_rows = crate::mutation::returning::ReturningRows::new(
                context.query.source.relational.runtime,
            )?;
            for index in 0..pending.len() {
                let row = pending.get(index)?;
                let old = view_values(&target, row.old)?;
                let new = view_values(&target, row.new)?;
                let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
                    &context.mutation.preparation.referential.triggers,
                    &target.canonical_name,
                    uqa_sql::ast::TriggerEvent::Update,
                    Some(&old),
                    Some(&new),
                    &assigned_columns,
                )?
                else {
                    continue;
                };
                checks.validate(&final_new)?;
                affected += 1;
                if !stmt.returning.is_empty() {
                    returning_rows.push_to(
                        build_returning_value_row(
                            context.mutation.preparation.returning,
                            ReturningValueProjectionRow {
                                table: &target.canonical_name,
                                target_qualifier: &stmt.target_qualifier,
                                current: &final_new,
                                old: Some(&old),
                                new: Some(&final_new),
                                aliases: &stmt.returning_aliases,
                                context: row.context.as_ref(),
                            },
                            &stmt.returning,
                            params,
                            ctes,
                        )?,
                        context.mutation.preparation.returning.destination,
                    )?;
                }
            }
            crate::mutation::statement_end::end_command(
                &statement_commands,
                &context.mutation.preparation.referential.triggers,
                std::slice::from_ref(&view_statement),
                crate::mutation::triggers::events::AfterRowEvents::default(),
            )?;
            let result = finish_view_dml(
                &context.mutation.preparation.returning,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes,
                    supplemental_schema: source_rows.as_ref().map(crate::SharedSpill::row_schema),
                },
                returning_rows,
                affected,
            )?;

            Ok(result)
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    };
    crate::mutation::statement_end::finish_statement(
        context,
        params,
        &stmt.ctes,
        statement_scope.as_mut(),
        result,
    )
}
