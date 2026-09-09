//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution for views with INSTEAD OF triggers and rewrite rules.

use super::{
    build_join_spill_with_ctes, build_returning_value_row, count_view_source_qualifications,
    finish_view_dml, matching_source_context, materialize_view_rows, required_view_delete_columns,
    resolve_view_target, target_row, validate_dml_expression_qualifiers,
    validate_returning_alias_relations, view_document, BTreeSet, CteScope, DeletePlan,
    DmlReturningShape, Engine, ReturningValueProjectionRow, SQLError, SQLParam, SQLResult,
    ViewDmlSourceMatch, ViewSourceQualification,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub(in crate::sql::dml) fn run_view_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
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
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(
                engine.capture_statement_read_snapshot()?,
            ))
        }
        None => None,
    };
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let snapshot_engine = statement_snapshot
        .as_deref()
        .map(|snapshot| engine.statement_read_snapshot_engine(snapshot));
    let read_engine = snapshot_engine.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_command(
        read_engine,
        stmt.statement_privilege_subject.as_deref(),
        stmt.relations_bound,
    )?;
    if let Some(parent) = inherited_ctes {
        ctes.inherit_cte_bindings(parent);
    }
    ctes.set_command_cte_snapshot(statement_snapshot.clone());
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let row_independent_delete_qualification = if stmt.source.is_none()
        && !engine
            .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Delete)?
            .is_empty()
    {
        super::super::super::row_independent_mutation_qualification_count(
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
    let source_privilege_expressions = stmt
        .predicate
        .iter()
        .chain(stmt.returning.iter().map(|projection| &projection.expr))
        .collect::<Vec<_>>();
    if let Some(source) = stmt.source.as_deref() {
        crate::sql::select::ensure_select_privileges_for_source_expressions(
            source,
            &source_privilege_expressions,
            &source_scope,
        )?;
    }
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
    let mut outer_rule_batches = super::super::super::prepare_view_rule_batches(
        super::super::super::ViewRuleBatchRequest {
            engine,
            relations: &stmt.view_rule_relations,
            event: uqa_sql::ast::RuleEvent::Delete,
            rows: &rule_rows,
            params,
            scope: &snapshot,
            insert_plans: &[],
            update_plans: &[],
            document_relation: Some(&target.canonical_name),
        },
    )?;
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
