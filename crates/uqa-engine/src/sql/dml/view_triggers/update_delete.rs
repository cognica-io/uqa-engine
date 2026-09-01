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
    BTreeSet, CteScope, DeletePlan, DmlReturningShape, Engine, OwnedPhysicalRow,
    ReturningValueProjectionRow, SQLError, SQLParam, SQLResult, ScalarExpr, UpdatePlan, Value,
    ViewDmlTarget,
};

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

fn evaluate_view_update_assignments(
    engine: &Engine,
    target: &ViewDmlTarget,
    stmt: &UpdatePlan,
    required: Option<&BTreeSet<String>>,
    pending: &mut PendingViewUpdate,
    params: &[SQLParam],
    scope: &CteScope,
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
                engine,
                scope,
                &assignment.value,
                Some(&pending.evaluation_row),
                params,
            )?
        };
        pending.new[position] = coerce_view_value(target, position, value)?;
        pending
            .evaluated_assignments
            .insert(assignment.column.clone());
    }
    Ok(())
}

fn matching_source_context(
    engine: &Engine,
    target_row: &OwnedPhysicalRow,
    source_rows: Option<&uqa_execution::SharedSpill>,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ViewDmlSourceMatch>, SQLError> {
    let Some(source_rows) = source_rows else {
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(target_row), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        return Ok(qualifies.then_some(ViewDmlSourceMatch::TargetOnly));
    };
    for source in source_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?
    {
        let source = source.map_err(crate::sql::select::physical_exec_error)?;
        let joined = dml_join_rows(target_row, &source);
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(&joined), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        if qualifies {
            return Ok(Some(ViewDmlSourceMatch::Source(source)));
        }
    }
    Ok(None)
}

fn view_qualification_references_target(
    target: &ViewDmlTarget,
    target_qualifier: &str,
    predicate: Option<&ScalarExpr>,
) -> bool {
    let Some(predicate) = predicate else {
        return false;
    };
    if crate::sql::select::expr_contains_subquery(predicate) {
        return true;
    }
    if crate::sql::select::expr_qualifiers(predicate)
        .iter()
        .any(|qualifier| {
            qualifier.eq_ignore_ascii_case(target_qualifier)
                || qualifier.eq_ignore_ascii_case(&target.canonical_name)
        })
    {
        return true;
    }
    if !crate::sql::select::expr_has_unqualified_column(predicate) {
        return false;
    }
    let mut columns = BTreeSet::new();
    !predicate.collect_columns(&mut columns)
        || columns.iter().any(|column| target.columns.contains(column))
}

struct ViewSourceQualification<'a> {
    engine: &'a Engine,
    target: &'a ViewDmlTarget,
    target_qualifier: &'a str,
    predicate: Option<&'a ScalarExpr>,
    candidates: &'a [Vec<Value>],
    source_rows: &'a uqa_execution::SharedSpill,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
}

fn count_view_source_qualifications(
    context: ViewSourceQualification<'_>,
) -> Result<usize, SQLError> {
    let ViewSourceQualification {
        engine,
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
            .map_err(crate::sql::select::physical_exec_error)?
        {
            let source = source.map_err(crate::sql::select::physical_exec_error)?;
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(engine, ctes, predicate, Some(&source), params)
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
            .map_err(crate::sql::select::physical_exec_error)?
        {
            let source = source.map_err(crate::sql::select::physical_exec_error)?;
            let joined = dml_join_rows(&physical, &source);
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(engine, ctes, predicate, Some(&joined), params)
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
pub(in crate::sql::dml) fn run_view_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
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
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                false,
                &assigned_columns,
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let row_independent_update_qualification = if stmt.source.is_none()
        && !engine
            .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Update)?
            .is_empty()
    {
        super::super::row_independent_mutation_qualification_count(
            read_engine,
            stmt.predicate.as_ref(),
            params,
            &ctes,
        )?
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
        )?
    {
        validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
        let rule_batch = crate::sql::rules::prepare_rule_batch(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
            Vec::new(),
        )?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
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
            engine,
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
    let source_rows = stmt
        .source
        .as_deref()
        .map(|source| build_join_spill_with_ctes(read_engine, source, params, &mut source_scope))
        .transpose()?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_rows
            .as_ref()
            .map(uqa_execution::SharedSpill::row_schema),
    )?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let required_columns = (!original_query_survives)
        .then(|| required_view_update_columns(engine, &target, stmt))
        .transpose()?
        .flatten();
    let candidates = materialize_view_rows(
        read_engine,
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
                engine: read_engine,
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
        Some(crate::sql::rules::relation_condition_row_columns(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
        )?)
    } else {
        None
    };
    let mut pending = Vec::new();
    for old in candidates {
        let physical = target_row(&target, &stmt.target_qualifier, &old)?;
        let Some(source_match) = matching_source_context(
            read_engine,
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
            read_engine,
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
            Ok(crate::sql::rules::RuleRowImage {
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
    let mut rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
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
                read_engine,
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
                    Ok(crate::sql::rules::RuleRowImage {
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
                Ok(crate::sql::rules::RuleRowImage {
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
        let mut outer_rule_batches =
            super::super::prepare_view_rule_batches(super::super::ViewRuleBatchRequest {
                engine,
                relations: &stmt.view_rule_relations,
                event: uqa_sql::ast::RuleEvent::Update,
                rows: &outer_rule_rows,
                params,
                scope: &snapshot,
                insert_plans: &[],
                update_plans: &stmt.view_rule_update_plans,
                document_relation: Some(&target.canonical_name),
            })?;
        outer_rule_batches.configure_action_qualification(Some(update_qualification_count));
        let outer_outcome = outer_rule_batches
            .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if outcome.returning.is_some() && outer_outcome.returning.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot have RETURNING lists in multiple rules".into(),
            });
        }
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: source_rows
                        .as_ref()
                        .map(uqa_execution::SharedSpill::row_schema),
                },
            );
        }
        if let Some(returning) = outer_outcome.returning {
            return returning.project(
                engine,
                params,
                &ctes,
                source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            );
        }
        return finish_view_dml(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
            Vec::new(),
            0,
        );
    }
    let rule_returning = rule_batch.execute_actions(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
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
        let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
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
                engine,
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    )?;
    let result = finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        },
        returning_rows,
        affected,
    )?;
    if let Some(rule_returning) = rule_returning {
        return rule_returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
        );
    }
    Ok(result)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub(in crate::sql::dml) fn run_view_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
    }
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Delete,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Delete,
                false,
                &[],
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let row_independent_delete_qualification = if stmt.source.is_none()
        && !engine
            .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Delete)?
            .is_empty()
    {
        super::super::row_independent_mutation_qualification_count(
            read_engine,
            stmt.predicate.as_ref(),
            params,
            &ctes,
        )?
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
        )?
    {
        validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
        let rule_batch = crate::sql::rules::prepare_rule_batch(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
            Vec::new(),
        )?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
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
            engine,
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
    let source_rows = stmt
        .source
        .as_deref()
        .map(|source| build_join_spill_with_ctes(read_engine, source, params, &mut source_scope))
        .transpose()?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_rows
            .as_ref()
            .map(uqa_execution::SharedSpill::row_schema),
    )?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let required_columns = if !original_query_survives && stmt.view_rule_relations.is_empty() {
        required_view_delete_columns(engine, &target, stmt)?
    } else {
        None
    };
    let candidates = materialize_view_rows(
        read_engine,
        &target,
        required_columns.as_ref(),
        params,
        &mut target_scope,
    )?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let source_delete_qualification_count = source_rows
        .as_ref()
        .map(|source_rows| {
            count_view_source_qualifications(ViewSourceQualification {
                engine: read_engine,
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
    let mut pending = Vec::new();
    for old in candidates {
        let physical = target_row(&target, &stmt.target_qualifier, &old)?;
        let Some(source_match) = matching_source_context(
            read_engine,
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
        pending.push((old, source_context));
    }
    let rule_rows = pending
        .iter()
        .map(|(old, source_context)| {
            Ok(crate::sql::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: Some(view_document(&target, old)?),
                new_storage_table: None,
                new_doc_id: None,
                new: None,
                context: source_context.clone(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut outer_rule_batches =
        super::super::prepare_view_rule_batches(super::super::ViewRuleBatchRequest {
            engine,
            relations: &stmt.view_rule_relations,
            event: uqa_sql::ast::RuleEvent::Delete,
            rows: &rule_rows,
            params,
            scope: &snapshot,
            insert_plans: &[],
            update_plans: &[],
            document_relation: Some(&target.canonical_name),
        })?;
    let mut rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Delete,
        rule_rows,
    )?;
    let action_qualification_count = source_delete_qualification_count
        .or(row_independent_delete_qualification)
        .unwrap_or_else(|| rule_batch.event_row_count());
    outer_rule_batches.configure_action_qualification(Some(action_qualification_count));
    rule_batch.set_action_qualification_count(action_qualification_count);
    let outer_rule_outcome = outer_rule_batches
        .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
    let rule_outcome = rule_batch.execute_actions_with_affected(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    if rule_outcome.returning.is_some() && outer_rule_outcome.returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
    for (index, (old, source_context)) in pending.into_iter().enumerate() {
        if rule_batch.suppresses(index) {
            continue;
        }
        if crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerEvent::Delete,
            Some(&old),
            None,
            &[],
        )?
        .is_none()
        {
            continue;
        }
        affected += 1;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_value_row(
                engine,
                ReturningValueProjectionRow {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    current: &old,
                    old: Some(&old),
                    new: None,
                    aliases: &stmt.returning_aliases,
                    context: source_context.as_ref(),
                },
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
    }
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let result = finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        },
        returning_rows,
        affected,
    )?;
    if let Some(rule_returning) = rule_outcome.returning {
        return rule_returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
        );
    }
    if let Some(rule_returning) = outer_rule_outcome.returning {
        return rule_returning.project(
            engine,
            params,
            &ctes,
            source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        );
    }
    Ok(result)
}
