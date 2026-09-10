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
    triggers::context::TriggerContext,
    views::commands::{materialize_view_rows, target_row, SourceOutputPruning},
};
use crate::query::{sources::build_join_spill_with_ctes, CteScope};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::{
    plan::{MergePlan, MergeWhenPlan, ViewCheckPlan},
    semantics::view_mutation::{
        coerce_view_value, resolve_view_target, target_columns, ViewMutationTarget as ViewDmlTarget,
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
            for assignment in assignments {
                let position = input
                    .target
                    .columns
                    .iter()
                    .position(|column| column == &assignment.column)
                    .ok_or_else(|| SQLError::UnknownColumn(assignment.column.clone()))?;
                let value = evaluate_view_assignment(
                    input.assignment,
                    input.target,
                    position,
                    &assignment.value,
                    input.action_row,
                    input.params,
                    input.ctes,
                )?;
                new[position] = value;
            }
            Ok(SelectedViewMergeAction::Update {
                old,
                new,
                updated_columns: assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
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
    explicit_columns: &[String],
    expressions: &[ScalarExpr],
) -> Result<SelectedViewMergeAction, SQLError> {
    let columns = target_columns(input.target, explicit_columns, "INSERT")?;
    let mut new = vec![Value::Null; input.target.columns.len()];
    for (column, expression) in columns.iter().zip(expressions) {
        let position = input
            .target
            .columns
            .iter()
            .position(|candidate| candidate == column)
            .ok_or_else(|| SQLError::UnknownColumn(column.clone()))?;
        new[position] = evaluate_view_assignment(
            input.assignment,
            input.target,
            position,
            expression,
            input.action_row,
            input.params,
            input.ctes,
        )?;
    }
    Ok(SelectedViewMergeAction::Insert { new })
}

fn evaluate_view_assignment<S: Clone + 'static>(
    assignment: MutationAssignmentContext<'_, S>,
    target: &ViewDmlTarget,
    position: usize,
    expression: &ScalarExpr,
    row: &OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Value, SQLError> {
    let value = if matches!(expression, ScalarExpr::Default) {
        Value::Null
    } else {
        eval_mutation_expr(assignment.expressions, ctes, expression, Some(row), params)?
    };
    coerce_view_value(assignment.assignment, target, position, value)
}

struct ViewMergeActionContext<'a, S: Clone + 'static> {
    assignment: MutationAssignmentContext<'a, S>,
    triggers: &'a TriggerContext<'a>,
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
    validate_view_merge_checks(context, &final_new)?;
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
    validate_view_merge_checks(context, &final_new)?;
    let returning =
        build_action_returning(context, pair, &final_new, None, Some(&final_new), "INSERT")?;
    Ok(ViewMergeActionResult {
        affected: true,
        returning,
    })
}

fn validate_view_merge_checks<S: Clone + 'static>(
    context: &ViewMergeActionContext<'_, S>,
    values: &[Value],
) -> Result<(), SQLError> {
    if context.plan.view_checks.is_empty() {
        return Ok(());
    }
    let row = target_row(context.target, &context.plan.target_qualifier, values)?;
    for ViewCheckPlan { view, predicate } in &context.plan.view_checks {
        let value = eval_mutation_expr(
            context.assignment.expressions,
            context.ctes,
            predicate,
            Some(&row),
            context.params,
        )?;
        if !uqa_sql::expr::truthy(&value) {
            let name = uqa_core::RelationIdentity::from_legacy_name(view)
                .map_or_else(|_| view.clone(), |relation| relation.name);
            return Err(SQLError::Routine {
                sqlstate: "44000".into(),
                message: format!("new row violates check option for view \"{name}\""),
            });
        }
    }
    Ok(())
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
    events.fire_before(triggers, &target.canonical_name)?;
    let execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<SQLResult, SQLError> {
            let read_mutation = &read_context.mutation;
            let mut ctes = read_mutation
                .scopes
                .command_scope(plan.statement_privilege_subject.as_deref(), false)?;
            if let Some(parent) = inherited_ctes {
                ctes.inherit_cte_bindings(parent);
            }
            ctes.set_command_cte_snapshot(statement_snapshot.clone());
            crate::query::cte::materialize_plan_ctes(
                context.query.source.ctes,
                &plan.ctes,
                params,
                &mut ctes,
            )?;
            ctes.scalar_subqueries.clone_from(&plan.subqueries);
            let source_privilege_expressions =
                uqa_sql::semantics::view_privileges::merge_privilege_expressions(plan);
            crate::query::privileges::ensure_select_privileges_for_source_expressions(
                &plan.source,
                &source_privilege_expressions,
                &ctes,
            )?;
            let source_rows = build_join_spill_with_ctes(
                &read_context.query.source,
                &plan.source,
                params,
                &mut ctes,
            )?;
            let mut target_scope = ctes.returning_statement_snapshot_scope();
            let candidates = materialize_view_rows(
                &read_context.query,
                prune,
                &target,
                None,
                params,
                &mut target_scope,
            )?;
            let snapshot = ctes.returning_statement_snapshot_scope();
            let pairings = build_view_merge_pairings(PairingInput {
                expressions: read_mutation.preparation.referential.assignment.expressions,
                runtime: read_context.query.source.relational.runtime,
                target: &target,
                plan,
                candidates: &candidates,
                source_rows: &source_rows,
                params,
                ctes: &snapshot,
            })?;
            let source_relation = uqa_sql::ast::InternalRelationId::allocate();
            let action_context = ViewMergeActionContext {
                assignment,
                triggers,
                returning,
                target: &target,
                plan,
                source_schema: source_rows.row_schema(),
                source_relation,
                params,
                ctes: &snapshot,
            };
            let (affected, returning_rows) = execute_view_merge_pairs(&action_context, &pairings)?;
            events.fire_after(triggers, &target.canonical_name)?;
            super::returning::finish_view_merge_returning(
                returning,
                super::returning::ViewMergeReturningResult {
                    stmt: plan,
                    source_schema: source_rows.row_schema(),
                    source_relation,
                    params,
                    ctes: &ctes,
                    rows: returning_rows,
                    affected,
                },
            )
        };
    match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
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
