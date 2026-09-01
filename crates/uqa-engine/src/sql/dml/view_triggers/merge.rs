//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `MERGE` execution for views whose selected actions use `INSTEAD OF` triggers.

use super::super::{
    build_join_spill_with_ctes, dml_join_rows, eval_mutation_expr, merge_source_index_value,
    validate_returning_alias_relations, BTreeSet, CteScope, Engine, MergePairKind, MergePlan,
    MergeWhenPlan, OwnedPhysicalRow, PhysicalRow, RowSchema, SQLError, SQLParam, SQLResult,
    ScalarExpr, Value, ViewCheckPlan,
};
use super::{
    coerce_view_value, materialize_view_rows, resolve_view_target, target_columns, target_row,
    ViewDmlTarget,
};

mod codec;

use codec::{decode_view_merge_pair, push_view_merge_pair, view_merge_pair_schema, ViewMergePair};

struct ViewMergeEvents {
    insert: bool,
    update: bool,
    delete: bool,
    updated_columns: Vec<String>,
}

impl ViewMergeEvents {
    fn from_plan(plan: &MergePlan) -> Self {
        let insert = plan
            .when_clauses
            .iter()
            .any(|clause| matches!(clause, MergeWhenPlan::InsertNotMatched { .. }));
        let update = plan.when_clauses.iter().any(|clause| {
            matches!(
                clause,
                MergeWhenPlan::UpdateMatched { .. }
                    | MergeWhenPlan::UpdateNotMatchedBySource { .. }
            )
        });
        let delete = plan.when_clauses.iter().any(|clause| {
            matches!(
                clause,
                MergeWhenPlan::DeleteMatched { .. }
                    | MergeWhenPlan::DeleteNotMatchedBySource { .. }
            )
        });
        let updated_columns = plan
            .when_clauses
            .iter()
            .filter_map(|clause| match clause {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => Some(assignments),
                _ => None,
            })
            .flatten()
            .map(|assignment| assignment.column.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            insert,
            update,
            delete,
            updated_columns,
        }
    }

    fn has_before_statement_trigger(&self, engine: &Engine, view: &str) -> Result<bool, SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled
                && !engine
                    .triggers_for(
                        view,
                        uqa_sql::ast::TriggerTiming::Before,
                        event,
                        false,
                        columns,
                    )?
                    .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn fire_before(&self, engine: &Engine, view: &str) -> Result<(), SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled {
                crate::sql::triggers::fire_statement_triggers(
                    engine,
                    view,
                    uqa_sql::ast::TriggerTiming::Before,
                    event,
                    columns,
                )?;
            }
        }
        Ok(())
    }

    fn fire_after(&self, engine: &Engine, view: &str) -> Result<(), SQLError> {
        for (enabled, event, columns) in [
            (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
            (
                self.update,
                uqa_sql::ast::TriggerEvent::Update,
                self.updated_columns.as_slice(),
            ),
            (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
        ] {
            if enabled {
                crate::sql::triggers::fire_statement_triggers(
                    engine,
                    view,
                    uqa_sql::ast::TriggerTiming::After,
                    event,
                    columns,
                )?;
            }
        }
        Ok(())
    }

    fn before_order(&self) -> [(bool, uqa_sql::ast::TriggerEvent, &[String]); 3] {
        [
            (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
            (
                self.update,
                uqa_sql::ast::TriggerEvent::Update,
                self.updated_columns.as_slice(),
            ),
            (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
        ]
    }
}

fn validate_view_merge_targets(target: &ViewDmlTarget, plan: &MergePlan) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let columns = assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
                let _ = target_columns(target, &columns, "UPDATE")?;
            }
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                let implicit = columns.is_empty();
                let columns = target_columns(target, columns, "INSERT")?;
                if values.len() > columns.len() || (!implicit && values.len() != columns.len()) {
                    return Err(SQLError::TypeMismatch(format!(
                        "MERGE INSERT row width {} != column count {}",
                        values.len(),
                        columns.len()
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

struct PairingInput<'a> {
    engine: &'a Engine,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    candidates: &'a [Vec<Value>],
    source_rows: &'a uqa_execution::SharedSpill,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
}

fn build_view_merge_pairings(
    input: PairingInput<'_>,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let PairingInput {
        engine,
        target,
        plan,
        candidates,
        source_rows,
        params,
        ctes,
    } = input;
    let schema = view_merge_pair_schema(source_rows.row_schema());
    let work_mem = crate::sql::select::physical_work_mem_bytes(engine.query_runtime_view())?.max(1);
    let mut pairings = uqa_execution::SpillBuffer::new(work_mem);
    let mut matched_source = uqa_execution::ExactRowSet::new(work_mem);
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
            let visible = eval_mutation_expr(engine, ctes, predicate, Some(&target_row), params)?;
            if !uqa_sql::expr::truthy(&visible) {
                continue;
            }
        }
        let mut matched = false;
        for (index, source) in source_rows
            .read_rows()
            .map_err(crate::sql::select::physical_exec_error)?
            .enumerate()
        {
            let source = source.map_err(crate::sql::select::physical_exec_error)?;
            let joined = dml_join_rows(&target_row, &source);
            let value =
                eval_mutation_expr(engine, ctes, &plan.join_condition, Some(&joined), params)?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
            matched = true;
            let index = merge_source_index_value(index);
            let _ = matched_source
                .insert_values(std::slice::from_ref(&index))
                .map_err(crate::sql::select::physical_exec_error)?;
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
        .map_err(crate::sql::select::physical_exec_error)?
        .enumerate()
    {
        let source = source.map_err(crate::sql::select::physical_exec_error)?;
        let index = merge_source_index_value(index);
        if !matched_source
            .contains_values(std::slice::from_ref(&index))
            .map_err(crate::sql::select::physical_exec_error)?
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
        .map_err(crate::sql::select::physical_exec_error)
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

struct ActionSelection<'a> {
    engine: &'a Engine,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    pair: &'a ViewMergePair,
    action_row: &'a OwnedPhysicalRow,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
}

fn select_view_merge_action(
    input: ActionSelection<'_>,
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
                input.engine,
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

fn selected_clause_action(
    input: &ActionSelection<'_>,
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
                    input.engine,
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

fn build_view_merge_insert(
    input: &ActionSelection<'_>,
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
            input.engine,
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

fn evaluate_view_assignment(
    engine: &Engine,
    target: &ViewDmlTarget,
    position: usize,
    expression: &ScalarExpr,
    row: &OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Value, SQLError> {
    let value = if matches!(expression, ScalarExpr::Default) {
        Value::Null
    } else {
        eval_mutation_expr(engine, ctes, expression, Some(row), params)?
    };
    coerce_view_value(engine, target, position, value)
}

struct ViewMergeActionContext<'a> {
    engine: &'a Engine,
    target: &'a ViewDmlTarget,
    plan: &'a MergePlan,
    source_schema: &'a RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
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

fn execute_selected_action(
    context: &ViewMergeActionContext<'_>,
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

fn execute_view_merge_update(
    context: &ViewMergeActionContext<'_>,
    pair: &ViewMergePair,
    old: &[Value],
    new: &[Value],
    updated_columns: &[String],
) -> Result<ViewMergeActionResult, SQLError> {
    let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
        context.engine,
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

fn execute_view_merge_delete(
    context: &ViewMergeActionContext<'_>,
    pair: &ViewMergePair,
    old: &[Value],
) -> Result<ViewMergeActionResult, SQLError> {
    if crate::sql::triggers::fire_instead_of_row_triggers(
        context.engine,
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

fn execute_view_merge_insert(
    context: &ViewMergeActionContext<'_>,
    pair: &ViewMergePair,
    new: &[Value],
) -> Result<ViewMergeActionResult, SQLError> {
    let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
        context.engine,
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

fn validate_view_merge_checks(
    context: &ViewMergeActionContext<'_>,
    values: &[Value],
) -> Result<(), SQLError> {
    if context.plan.view_checks.is_empty() {
        return Ok(());
    }
    let row = target_row(context.target, &context.plan.target_qualifier, values)?;
    for ViewCheckPlan { view, predicate } in &context.plan.view_checks {
        let value = eval_mutation_expr(
            context.engine,
            context.ctes,
            predicate,
            Some(&row),
            context.params,
        )?;
        if !uqa_sql::expr::truthy(&value) {
            let name = crate::RelationIdentity::from_legacy_name(view)
                .map_or_else(|_| view.clone(), |relation| relation.name);
            return Err(SQLError::Routine {
                sqlstate: "44000".into(),
                message: format!("new row violates check option for view \"{name}\""),
            });
        }
    }
    Ok(())
}

fn build_action_returning(
    context: &ViewMergeActionContext<'_>,
    pair: &ViewMergePair,
    current: &[Value],
    old: Option<&[Value]>,
    new: Option<&[Value]>,
    action: &str,
) -> Result<Option<OwnedPhysicalRow>, SQLError> {
    if context.plan.returning.is_empty() {
        return Ok(None);
    }
    super::super::merge::build_view_merge_returning_row(
        context.engine,
        super::super::merge::ViewMergeReturningRow {
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

fn execute_view_merge_pairs(
    context: &ViewMergeActionContext<'_>,
    pairings: &uqa_execution::SharedSpill,
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
        .map_err(crate::sql::select::physical_exec_error)?
    {
        let pair = decode_view_merge_pair(pair.map_err(crate::sql::select::physical_exec_error)?)?;
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
            engine: context.engine,
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

pub(in crate::sql) fn run_view_merge_inner(
    engine: &Engine,
    plan: &MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &plan.target)?;
    validate_view_merge_targets(&target, plan)?;
    let mut analysis_scope = CteScope::new_for_current_routine(engine);
    analysis_scope
        .scalar_subqueries
        .clone_from(&plan.subqueries);
    let source_schema = crate::sql::select::analyze_source_plan_schema(
        engine,
        &plan.source,
        params,
        &analysis_scope,
        None,
    )?;
    super::super::view_automatic::validate_public_merge_targets(engine, plan)?;
    super::super::view_automatic::validate_public_merge_contract(engine, plan, &source_schema)?;
    validate_returning_alias_relations(
        &plan.target_qualifier,
        &plan.returning_aliases,
        Some(&source_schema),
    )?;
    let null_target = target_row(
        &target,
        &plan.target_qualifier,
        &vec![Value::Null; target.columns.len()],
    )?;
    super::super::merge::validate_merge_action_scopes(
        engine,
        plan,
        &null_target.schema,
        &source_schema,
        params,
    )?;
    let events = ViewMergeEvents::from_plan(plan);
    let statement_snapshot = events
        .has_before_statement_trigger(engine, &target.canonical_name)?
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    events.fire_before(engine, &target.canonical_name)?;
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    ctes.scalar_subqueries.clone_from(&plan.subqueries);
    let source_rows = build_join_spill_with_ctes(read_engine, &plan.source, params, &mut ctes)?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let candidates = materialize_view_rows(read_engine, &target, None, params, &mut target_scope)?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let pairings = build_view_merge_pairings(PairingInput {
        engine: read_engine,
        target: &target,
        plan,
        candidates: &candidates,
        source_rows: &source_rows,
        params,
        ctes: &snapshot,
    })?;
    let source_relation = uqa_sql::ast::InternalRelationId::allocate();
    let action_context = ViewMergeActionContext {
        engine,
        target: &target,
        plan,
        source_schema: source_rows.row_schema(),
        source_relation,
        params,
        ctes: &snapshot,
    };
    let (affected, returning) = execute_view_merge_pairs(&action_context, &pairings)?;
    events.fire_after(engine, &target.canonical_name)?;
    super::super::merge::finish_view_merge_returning(
        engine,
        super::super::merge::ViewMergeReturningResult {
            stmt: plan,
            source_schema: source_rows.row_schema(),
            source_relation,
            params,
            ctes: &ctes,
            rows: returning,
            affected,
        },
    )
}
