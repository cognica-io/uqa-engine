//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE pairing and INSTEAD OF trigger execution for view targets.
use super::codec::{merge_source_index_value, MergePairKind};
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::mutation::{
    assignment::MutationAssignmentContext,
    expressions::eval_mutation_expr,
    returning::ReturningExecutionContext,
    rows::{context::MutationExpressionContext, join_rows as dml_join_rows},
    statement_end,
    triggers::context::TriggerContext,
    views::commands::{materialize_view_rows, target_row, SourceOutputPruning, TriggerViewChecks},
};
use crate::query::{scope::StatementCommands, sources::build_join_spill_with_ctes, CteScope};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::{
    plan::{MergePlan, MergeWhenPlan},
    semantics::view_mutation::{
        resolve_view_target, target_columns, ViewMutationTarget as ViewDmlTarget,
    },
    SQLError, SQLParam, SQLResult, ScalarExpr,
};
mod codec;
use codec::{decode_view_merge_pair, push_view_merge_pair, view_merge_pair_schema, ViewMergePair};

struct PairingInput<'a, S: Clone + 'static> {
    expressions: MutationExpressionContext<'a, S>,
    runtime: crate::query::runtime::QueryRuntimeView<'a>,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    candidates: &'a [Vec<Value>],
    source_rows: &'a crate::SharedSpill,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
}

fn build_view_merge_pairings<S: Clone + 'static>(
    input: PairingInput<'_, S>,
) -> Result<crate::SharedSpill, SQLError> {
    let PairingInput {
        expressions,
        runtime,
        target,
        plan,
        candidates,
        source_rows,
        params,
        ctes,
    } = input;
    let schema = view_merge_pair_schema(source_rows.row_schema());
    let work_mem = crate::query::projection::physical_work_mem_bytes(runtime)?.max(1);
    let mut pairings = crate::SpillBuffer::new(work_mem);
    let mut matched_source = crate::ExactRowSet::new(work_mem);
    let has_source_missing = plan.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        )
    });
    let null_source = OwnedPhysicalRow::new(
        source_rows.row_schema().clone(),
        PhysicalRow::nulls(source_rows.row_schema().physical_width()),
    );
    for values in candidates {
        let target_row = target_row(target, &plan.target_qualifier, values)?;
        if let Some(predicate) = &plan.target_predicate {
            let visible =
                eval_mutation_expr(expressions, ctes, predicate, Some(&target_row), params)?;
            if !uqa_sql::expr::truthy(&visible) {
                continue;
            }
        }
        let mut matched = false;
        for (index, source) in source_rows
            .read_rows()
            .map_err(crate::physical::physical_exec_error)?
            .enumerate()
        {
            let source = source.map_err(crate::physical::physical_exec_error)?;
            let joined = dml_join_rows(&target_row, &source);
            let value = eval_mutation_expr(
                expressions,
                ctes,
                &plan.join_condition,
                Some(&joined),
                params,
            )?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
            matched = true;
            let index = merge_source_index_value(index);
            let _ = matched_source
                .insert_values(std::slice::from_ref(&index))
                .map_err(crate::physical::physical_exec_error)?;
            push_view_merge_pair(
                &mut pairings,
                &schema,
                MergePairKind::Matched,
                Some(values),
                &source,
            )?;
        }
        if !matched && has_source_missing {
            push_view_merge_pair(
                &mut pairings,
                &schema,
                MergePairKind::NotMatchedBySource,
                Some(values),
                &null_source,
            )?;
        }
    }
    for (index, source) in source_rows
        .read_rows()
        .map_err(crate::physical::physical_exec_error)?
        .enumerate()
    {
        let source = source.map_err(crate::physical::physical_exec_error)?;
        let index = merge_source_index_value(index);
        if !matched_source
            .contains_values(std::slice::from_ref(&index))
            .map_err(crate::physical::physical_exec_error)?
        {
            push_view_merge_pair(
                &mut pairings,
                &schema,
                MergePairKind::NotMatchedByTarget,
                None,
                &source,
            )?;
        }
    }
    pairings
        .into_shared(schema)
        .map_err(crate::physical::physical_exec_error)
}

enum SelectedViewMergeAction {
    Nothing,
    Update {
        old: Vec<Value>,
        new: Vec<Value>,
        updated_columns: Vec<String>,
    },
    Delete {
        old: Vec<Value>,
    },
    Insert {
        new: Vec<Value>,
    },
}

fn clause_matches_kind(clause: &MergeWhenPlan, kind: MergePairKind) -> bool {
    match kind {
        MergePairKind::Matched => matches!(
            clause,
            MergeWhenPlan::UpdateMatched { .. }
                | MergeWhenPlan::DeleteMatched { .. }
                | MergeWhenPlan::NothingMatched { .. }
        ),
        MergePairKind::NotMatchedBySource => matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        ),
        MergePairKind::NotMatchedByTarget => matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. }
        ),
    }
}

struct ActionSelection<'a, S: Clone + 'static> {
    assignment: MutationAssignmentContext<'a, S>,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    pair: &'a ViewMergePair,
    action_row: &'a OwnedPhysicalRow,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
}

fn select_view_merge_action<S: Clone + 'static>(
    input: ActionSelection<'_, S>,
) -> Result<SelectedViewMergeAction, SQLError> {
    for clause in &input.plan.when_clauses {
        if !clause_matches_kind(clause, input.pair.kind) {
            continue;
        }
        let condition = match clause {
            MergeWhenPlan::UpdateMatched { condition, .. }
            | MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::InsertNotMatched { condition, .. }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => condition,
        };
        if let Some(condition) = condition {
            let value = eval_mutation_expr(
                input.assignment.expressions,
                input.ctes,
                condition,
                Some(input.action_row),
                input.params,
            )?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
        }
        return selected_clause_action(&input, clause);
    }
    Ok(SelectedViewMergeAction::Nothing)
}

fn selected_clause_action<S: Clone + 'static>(
    input: &ActionSelection<'_, S>,
    clause: &MergeWhenPlan,
) -> Result<SelectedViewMergeAction, SQLError> {
    match clause {
        MergeWhenPlan::UpdateMatched { assignments, .. }
        | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
            let old = input
                .pair
                .target
                .clone()
                .ok_or_else(|| SQLError::Internal("view MERGE update lost OLD".into()))?;
            let mut new = old.clone();
            for (assignment_index, assignment) in assignments.iter().enumerate() {
                let source = crate::mutation::assignment::AssignmentSource::evaluate(
                    input.assignment,
                    input.ctes,
                    assignment,
                    Some(input.action_row),
                    input.params,
                )?;
                for (target_position, target) in assignment.target.targets().iter().enumerate() {
                    let position = input
                        .target
                        .columns
                        .iter()
                        .position(|column| column == &target.column)
                        .ok_or_else(|| SQLError::UnknownColumn(target.column.clone()))?;
                    new[position] = crate::mutation::assignment::eval_typed_assignment_input(
                        input.assignment,
                        input.ctes,
                        crate::mutation::assignment::TypedAssignmentTarget {
                            target,
                            ty: input.target.types[position].as_ref(),
                            current: Some(&new[position]),
                            final_column_write: crate::mutation::assignment::final_column_write(
                                assignments,
                                assignment_index,
                                target_position,
                            ),
                        },
                        source.input(assignment.target.source_position(target_position)),
                        Some(input.action_row),
                        input.params,
                    )?;
                }
            }
            Ok(SelectedViewMergeAction::Update {
                old,
                new,
                updated_columns: assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names())
                    .map(str::to_owned)
                    .collect(),
            })
        }
        MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. } => {
            Ok(SelectedViewMergeAction::Delete {
                old: input
                    .pair
                    .target
                    .clone()
                    .ok_or_else(|| SQLError::Internal("view MERGE delete lost OLD".into()))?,
            })
        }
        MergeWhenPlan::InsertNotMatched {
            columns, values, ..
        } => build_view_merge_insert(input, columns, values),
        MergeWhenPlan::NothingMatched { .. }
        | MergeWhenPlan::NothingNotMatched { .. }
        | MergeWhenPlan::NothingNotMatchedBySource { .. } => Ok(SelectedViewMergeAction::Nothing),
    }
}

fn build_view_merge_insert<S: Clone + 'static>(
    input: &ActionSelection<'_, S>,
    explicit_columns: &[uqa_sql::ast::AssignmentTarget<ScalarExpr>],
    expressions: &[ScalarExpr],
) -> Result<SelectedViewMergeAction, SQLError> {
    let columns = target_columns(input.target, explicit_columns, "INSERT")?;
    let mut new = vec![Value::Null; input.target.columns.len()];
    for (index, (column, expression)) in columns.iter().zip(expressions).enumerate() {
        let position = input
            .target
            .columns
            .iter()
            .position(|candidate| candidate == &column.column)
            .ok_or_else(|| SQLError::UnknownColumn(column.column.clone()))?;
        new[position] = evaluate_view_assignment(
            input.assignment,
            input.target,
            position,
            column,
            Some(&new[position]),
            !columns[index + 1..]
                .iter()
                .any(|next| next.column == column.column),
            expression,
            input.action_row,
            input.params,
            input.ctes,
        )?;
    }
    Ok(SelectedViewMergeAction::Insert { new })
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserves original MERGE input and composed view target"
)]
fn evaluate_view_assignment<S: Clone + 'static>(
    assignment: MutationAssignmentContext<'_, S>,
    target: &ViewDmlTarget,
    position: usize,
    assignment_target: &uqa_sql::ast::AssignmentTarget<ScalarExpr>,
    current: Option<&Value>,
    final_column_write: bool,
    expression: &ScalarExpr,
    row: &OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Value, SQLError> {
    crate::mutation::assignment::eval_typed_assignment(
        assignment,
        ctes,
        crate::mutation::assignment::TypedAssignmentTarget {
            target: assignment_target,
            ty: target.types[position].as_ref(),
            current,
            final_column_write,
        },
        expression,
        Some(row),
        params,
    )
}

struct ViewMergeActionContext<'a, S: Clone + 'static> {
    assignment: MutationAssignmentContext<'a, S>,
    triggers: &'a TriggerContext<'a>,
    checks: TriggerViewChecks<'a, S>,
    returning: &'a ReturningExecutionContext<'a, S>,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    source_schema: &'a RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
}

struct ViewMergeActionResult {
    affected: bool,
    returning: Option<OwnedPhysicalRow>,
}

impl ViewMergeActionResult {
    fn suppressed() -> Self {
        Self {
            affected: false,
            returning: None,
        }
    }
}

fn execute_selected_action<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pair: &ViewMergePair,
    action: SelectedViewMergeAction,
) -> Result<ViewMergeActionResult, SQLError> {
    match action {
        SelectedViewMergeAction::Nothing => Ok(ViewMergeActionResult::suppressed()),
        SelectedViewMergeAction::Update {
            old,
            new,
            updated_columns,
        } => execute_view_merge_update(context, pair, &old, &new, &updated_columns),
        SelectedViewMergeAction::Delete { old } => execute_view_merge_delete(context, pair, &old),
        SelectedViewMergeAction::Insert { new } => execute_view_merge_insert(context, pair, &new),
    }
}

fn execute_view_merge_update<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pair: &ViewMergePair,
    old: &[Value],
    new: &[Value],
    updated_columns: &[String],
) -> Result<ViewMergeActionResult, SQLError> {
    let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
        context.triggers,
        &context.target.canonical_name,
        uqa_sql::ast::TriggerEvent::Update,
        Some(old),
        Some(new),
        updated_columns,
    )?
    else {
        return Ok(ViewMergeActionResult::suppressed());
    };
    context.checks.validate(&final_new)?;
    let returning = build_action_returning(
        context,
        pair,
        &final_new,
        Some(old),
        Some(&final_new),
        "UPDATE",
    )?;
    Ok(ViewMergeActionResult {
        affected: true,
        returning,
    })
}

fn execute_view_merge_delete<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pair: &ViewMergePair,
    old: &[Value],
) -> Result<ViewMergeActionResult, SQLError> {
    if crate::mutation::triggers::fire_instead_of_row_triggers(
        context.triggers,
        &context.target.canonical_name,
        uqa_sql::ast::TriggerEvent::Delete,
        Some(old),
        None,
        &[],
    )?
    .is_none()
    {
        return Ok(ViewMergeActionResult::suppressed());
    }
    let returning = build_action_returning(context, pair, old, Some(old), None, "DELETE")?;
    Ok(ViewMergeActionResult {
        affected: true,
        returning,
    })
}

fn execute_view_merge_insert<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pair: &ViewMergePair,
    new: &[Value],
) -> Result<ViewMergeActionResult, SQLError> {
    let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
        context.triggers,
        &context.target.canonical_name,
        uqa_sql::ast::TriggerEvent::Insert,
        None,
        Some(new),
        &[],
    )?
    else {
        return Ok(ViewMergeActionResult::suppressed());
    };
    context.checks.validate(&final_new)?;
    let returning =
        build_action_returning(context, pair, &final_new, None, Some(&final_new), "INSERT")?;
    Ok(ViewMergeActionResult {
        affected: true,
        returning,
    })
}

fn build_action_returning<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pair: &ViewMergePair,
    current: &[Value],
    old: Option<&[Value]>,
    new: Option<&[Value]>,
    action: &str,
) -> Result<Option<OwnedPhysicalRow>, SQLError> {
    if context.plan.returning.is_empty() {
        return Ok(None);
    }
    super::returning::build_view_merge_returning_row(
        context.returning,
        super::returning::ViewMergeReturningRow {
            table: &context.target.canonical_name,
            target_qualifier: &context.plan.target_qualifier,
            current,
            old,
            new,
            returning_aliases: &context.plan.returning_aliases,
            source_row: &pair.source,
            source_schema: context.source_schema,
            source_relation: context.source_relation,
            action,
        },
        &context.plan.returning,
        context.params,
        context.ctes,
    )
    .map(Some)
}

fn execute_view_merge_pairs<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    pairings: &crate::SharedSpill,
) -> Result<(u64, Vec<OwnedPhysicalRow>), SQLError> {
    let null_target = target_row(
        context.target,
        &context.plan.target_qualifier,
        &vec![Value::Null; context.target.columns.len()],
    )?;
    let mut affected = 0_u64;
    let mut returning = Vec::new();
    for pair in pairings
        .read_rows()
        .map_err(crate::physical::physical_exec_error)?
    {
        let pair = decode_view_merge_pair(pair.map_err(crate::physical::physical_exec_error)?)?;
        let target = pair
            .target
            .as_deref()
            .map(|values| target_row(context.target, &context.plan.target_qualifier, values))
            .transpose()?;
        let action_row = match pair.kind {
            MergePairKind::Matched => {
                dml_join_rows(target.as_ref().unwrap_or(&null_target), &pair.source)
            }
            MergePairKind::NotMatchedBySource => target.unwrap_or_else(|| null_target.clone()),
            MergePairKind::NotMatchedByTarget => pair.source.clone(),
        };
        let action = select_view_merge_action(ActionSelection {
            assignment: context.assignment,
            target: context.target,
            plan: context.plan,
            pair: &pair,
            action_row: &action_row,
            params: context.params,
            ctes: context.ctes,
        })?;
        let result = execute_selected_action(context, &pair, action)?;
        affected += u64::from(result.affected);
        if let Some(row) = result.returning {
            returning.push(row);
        }
    }
    Ok((affected, returning))
}

pub fn run_view_merge<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    plan: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let mutation = &context.mutation;
    let assignment = mutation.preparation.referential.assignment;
    let triggers = &mutation.preparation.referential.triggers;
    let returning = &mutation.preparation.returning;
    let PreparedViewMerge {
        target,
        events,
        statement_snapshot,
    } = prepare_view_merge(
        context.mutation.rules.views.rewrite,
        mutation.scopes,
        triggers,
        context.snapshots,
        plan,
        params,
        inherited_ctes,
    )?;
    let (statement_commands, _running_statement) =
        begin_view_merge_statement(inherited_ctes, triggers, &events, &target)?;
    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<SQLResult, SQLError> {
            let ctes = statement_scope.insert(view_merge_scope(
                context,
                read_context,
                plan,
                params,
                inherited_ctes,
                &statement_commands,
                statement_snapshot.clone(),
            )?);
            let ViewMergeRows {
                source_rows,
                pairings,
                snapshot,
            } = pair_view_merge_rows(read_context, prune, &target, plan, params, ctes)?;
            let source_relation = uqa_sql::ast::InternalRelationId::allocate();
            let supplied_columns =
                uqa_sql::semantics::merge::merge_supplied_columns(plan, &target.columns);
            let action_context = ViewMergeActionContext {
                assignment,
                triggers,
                checks: TriggerViewChecks {
                    expressions: assignment.expressions,
                    constraints: mutation.preparation.referential.constraints,
                    target: &target,
                    target_qualifier: &plan.target_qualifier,
                    checks: &plan.view_checks,
                    supplied: &supplied_columns,
                    params,
                    scope: &snapshot,
                },
                returning,
                target: &target,
                plan,
                source_schema: source_rows.row_schema(),
                source_relation,
                params,
                ctes: &snapshot,
            };
            let (affected, returning_rows) = execute_view_merge_pairs(&action_context, &pairings)?;
            statement_end::end_command(
                &statement_commands,
                triggers,
                &events.after_statements(&target.canonical_name),
                Vec::new(),
            )?;
            super::returning::finish_view_merge_returning(
                returning,
                super::returning::ViewMergeReturningResult {
                    stmt: plan,
                    source_schema: source_rows.row_schema(),
                    source_relation,
                    params,
                    ctes,
                    rows: returning_rows,
                    affected,
                },
            )
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }?;
    statement_end::finish_statement(context, params, &plan.ctes, statement_scope.as_mut())?;
    Ok(result)
}

/// The rows a view MERGE acts on.
struct ViewMergeRows<S: Clone + 'static> {
    source_rows: crate::SharedSpill,
    /// Each source row with the view row it matches, or with none.
    pairings: crate::SharedSpill,
    /// The statement's snapshot, which the actions read under.
    snapshot: CteScope<S>,
}

/// Read the MERGE's source and the view's rows under the statement's snapshot and pair them.
fn pair_view_merge_rows<S: Clone + Send + Sync + 'static>(
    read_context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    target: &ViewDmlTarget,
    plan: &MergePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<ViewMergeRows<S>, SQLError> {
    let source_privilege_expressions =
        uqa_sql::semantics::view_privileges::merge_privilege_expressions(plan);
    crate::query::privileges::ensure_select_privileges_for_source_expressions(
        &plan.source,
        &source_privilege_expressions,
        ctes,
    )?;
    let source_rows =
        build_join_spill_with_ctes(&read_context.query.source, &plan.source, params, ctes)?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let candidates = materialize_view_rows(
        &read_context.query,
        prune,
        target,
        None,
        params,
        &mut target_scope,
    )?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let pairings = build_view_merge_pairings(PairingInput {
        expressions: read_context
            .mutation
            .preparation
            .referential
            .assignment
            .expressions,
        runtime: read_context.query.source.relational.runtime,
        target,
        plan,
        candidates: &candidates,
        source_rows: &source_rows,
        params,
        ctes: &snapshot,
    })?;
    Ok(ViewMergeRows {
        source_rows,
        pairings,
        snapshot,
    })
}

/// The statement a view MERGE belongs to, with the BEFORE STATEMENT triggers of the view fired for the MERGE's actions.
fn begin_view_merge_statement<S: Clone>(
    inherited_ctes: Option<&CteScope<S>>,
    triggers: &TriggerContext<'_>,
    events: &super::statement_events::MergeStatementEvents,
    target: &ViewDmlTarget,
) -> Result<
    (
        std::sync::Arc<StatementCommands>,
        Option<statement_end::RunningStatement>,
    ),
    SQLError,
> {
    let (statement_commands, running_statement) = statement_end::statement_commands(inherited_ctes);
    statement_end::fire_before_statements(
        &statement_commands,
        triggers,
        &events.before_statements(&target.canonical_name),
    )?;
    Ok((statement_commands, running_statement))
}

/// The scope of a view MERGE's command, with the WITH items that run before the MERGE materialized.
fn view_merge_scope<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    read_context: &MutationStatementContext<'_, S>,
    plan: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
    statement_commands: &std::sync::Arc<StatementCommands>,
    statement_snapshot: Option<std::sync::Arc<S>>,
) -> Result<CteScope<S>, SQLError> {
    let mut ctes = read_context
        .mutation
        .scopes
        .command_scope(plan.statement_privilege_subject.as_ref(), false)?;
    if let Some(parent) = inherited_ctes {
        ctes.inherit_cte_bindings(parent);
    }
    ctes.set_statement_commands(std::sync::Arc::clone(statement_commands));
    ctes.set_command_cte_snapshot(statement_snapshot);
    crate::query::cte::materialize_command_ctes(
        context.query.source.ctes,
        &plan.ctes,
        || {
            uqa_sql::semantics::primary_command_cte_references(
                &plan.ctes,
                &plan.query_inputs(),
                plan.source_input(),
            )
        },
        params,
        &mut ctes,
    )?;
    ctes.scalar_subqueries.clone_from(&plan.subqueries);
    Ok(ctes)
}

struct PreparedViewMerge<S> {
    target: ViewDmlTarget,
    events: super::statement_events::MergeStatementEvents,
    statement_snapshot: Option<std::sync::Arc<S>>,
}
fn prepare_view_merge<S: Clone + 'static>(
    rewrite: uqa_sql::semantics::view_rewrite::context::ViewRewriteContext<'_>,
    scopes: &dyn crate::mutation::command_scope::CommandScopeSource<S>,
    triggers: &TriggerContext<'_>,
    snapshots: &dyn crate::query::statement::context::SnapshotSource<S>,
    plan: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<PreparedViewMerge<S>, SQLError> {
    let target = resolve_view_target(rewrite, &plan.target)?;
    let analysis_scope = super::analysis::merge_analysis_scope(scopes, plan, inherited_ctes)?;
    uqa_sql::semantics::view_mutation::validate_view_merge_contract(
        rewrite,
        &target,
        plan,
        params,
        &crate::query::binding::binding_context(&analysis_scope)?,
    )?;
    let events = super::statement_events::MergeStatementEvents::from_plan(plan);
    let has_before_statement_trigger =
        events.has_before_statement_trigger(triggers, &target.canonical_name)?;
    let statement_snapshot = crate::mutation::command_scope::capture_command_read_snapshot(
        snapshots,
        inherited_ctes,
        has_before_statement_trigger,
        &plan.ctes,
    )?;
    Ok(PreparedViewMerge {
        target,
        events,
        statement_snapshot,
    })
}
