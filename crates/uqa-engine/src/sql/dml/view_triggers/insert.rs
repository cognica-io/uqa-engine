//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution for views with `INSTEAD OF` triggers.

use super::{
    build_returning_value_row, coerce_view_value, eval_mutation_expr, finish_view_dml,
    resolve_view_target, run_suppressed_view_insert_rules, target_columns,
    validate_returning_alias_relations, values_from_result, view_document, BTreeSet, CteScope,
    DmlReturningShape, Engine, InsertPlan, ReturningValueProjectionRow, SQLError, SQLParam,
    SQLResult, ScalarExpr, Value,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub(in crate::sql::dml) fn run_view_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
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
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
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
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let suppressed_source_is_unused = stmt.source.is_some()
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Insert,
        )?;
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && (stmt.source.is_none() || suppressed_source_is_unused)
    {
        return run_suppressed_view_insert_rules(
            engine,
            read_engine,
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
            if let Some(required_columns) = crate::sql::rules::relation_rule_row_columns(
                engine,
                &target.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
            )? {
                let required_positions = columns
                    .iter()
                    .enumerate()
                    .filter_map(|(position, column)| {
                        required_columns.contains(column).then_some(position)
                    })
                    .collect::<BTreeSet<_>>();
                super::super::prune_unused_query_outputs(
                    &mut source,
                    &required_positions,
                    columns.len(),
                );
            }
        }
        let mut source_scope = ctes.returning_statement_snapshot_scope();
        values_from_result(crate::sql::select::execute_query_plan_with_ctes(
            read_engine,
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
                            eval_mutation_expr(read_engine, &snapshot, expression, None, params)
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
                new[target_position] =
                    coerce_view_value(engine, &target, target_position, value.clone())?;
            }
        }
        proposed_rows.push(new);
    }
    let rule_rows = proposed_rows
        .iter()
        .map(|new| {
            Ok(crate::sql::rules::RuleRowImage {
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
    let outer_rule_batches =
        super::super::prepare_view_rule_batches(super::super::ViewRuleBatchRequest {
            engine,
            relations: &stmt.view_rule_relations,
            event: uqa_sql::ast::RuleEvent::Insert,
            rows: &rule_rows,
            params,
            scope: &ctes,
            insert_plans: &stmt.view_rule_insert_plans,
            update_plans: &[],
            document_relation: Some(&target.canonical_name),
        })?;
    let rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
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
        let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
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
                engine,
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
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    let mut result = finish_view_dml(
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
        returning_rows,
        affected,
    )?;
    let rule_outcome = rule_batch.execute_actions_with_affected(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    let outer_rule_outcome = outer_rule_batches
        .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
    if rule_outcome.returning.is_some() && outer_rule_outcome.returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
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
                supplemental_schema: None,
            },
        );
    }
    if let Some(outer_returning) = outer_rule_outcome.returning {
        return outer_returning.project(engine, params, &ctes, None);
    }
    if !original_query_survives && rule_outcome.executed_action {
        result.affected_rows = rule_outcome.affected_rows;
    }
    if !original_query_survives && outer_rule_outcome.executed_action {
        result.affected_rows = outer_rule_outcome.affected_rows;
    }
    Ok(result)
}
