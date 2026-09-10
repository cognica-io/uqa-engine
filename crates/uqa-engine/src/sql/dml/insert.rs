//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use std::rc::Rc;
use std::sync::Arc;

use super::{
    dml_returning_result, dml_storage_error, document_supplied_id, eval_mutation_assignment,
    eval_mutation_expr, finish_mutation_publication, insert_identity_columns,
    partition_insert_target, persist_auto_increment_identity, prepare_auto_increment_identity,
    prepare_insert_identity, validate_mutation_columns, validate_returning_alias_relations,
    BTreeSet, ColumnType, ConflictActionPlan, ConflictPlan, CteScope, DmlReturningShape, DocId,
    Document, Engine, InsertConflictLocks, InsertPlan, MutationAssignmentTarget,
    MutationOverlayScope, MutationPublicationBatch, PreparedInsertConflict, SQLError, SQLParam,
    SQLResult,
};

mod codec;
mod staging;
mod view_rules;
use view_rules::{required_view_rule_insert_input_positions, view_rule_insert_column_type};

use codec::{decode_prepared_insert_spill_row, PreparedInsertSpillRow};

pub(in crate::sql) use staging::{
    apply_missing_column_defaults, refresh_insert_identity_after_trigger,
};
use staging::{apply_validated_prepared_insert, prepare_values_insert_row};

mod select_source;
use select_source::{
    insert_source_expression_rows, InsertSelectConsumer, InsertSelectIdentity, PreparedInsertSelect,
};

pub(in crate::sql) fn run_insert(
    engine: &Engine,
    mut stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_insert_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_insert_with_ctes(
    engine: &Engine,
    mut stmt: InsertPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_insert_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_insert_inner_with_ctes(engine, stmt, params, None)
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
fn run_insert_inner_with_ctes(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            let _ = super::view_privileges::ensure_insert(engine, stmt)?;
            let relation = crate::RelationIdentity::from_legacy_name(&stmt.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot change materialized view \"{}\"", relation.name),
            });
        }
        if super::view_automatic::has_instead_of_trigger(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Insert,
        )? || crate::sql::rules::relation_suppresses_original_query(
            engine,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Insert,
        )? {
            let _ = super::view_privileges::ensure_insert(engine, stmt)?;
            return super::view_triggers::run_view_insert_inner(
                engine,
                stmt,
                params,
                inherited_ctes,
            );
        }
        let rewritten =
            super::view_automatic::rewrite_insert_to_base(engine, stmt, params, inherited_ctes)?;
        return run_insert_inner_with_ctes(engine, &rewritten, params, inherited_ctes);
    }
    let _transition_capture_scope = crate::sql::triggers::TransitionCaptureScope::enter();
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let insert_rules = engine.rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Insert)?;
    let has_insert_rules = !insert_rules.is_empty();
    let has_view_insert_rules = !stmt.view_rule_relations.is_empty();
    let has_any_insert_rules = has_insert_rules || has_view_insert_rules;
    if stmt.on_conflict.is_some()
        && (has_insert_rules
            || !engine
                .rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Update)?
                .is_empty())
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "INSERT with ON CONFLICT clause cannot be used with table that has INSERT or UPDATE rules".into(),
        });
    }
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    crate::sql::rules::validate_rule_returning_contract(
        engine,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Insert,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        crate::sql::rules::validate_rule_returning_contract(
            engine,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Insert,
            !view_returning.returning.is_empty(),
        )?;
    }
    let prepared_statement = super::conflict::prepare_inference_predicate(engine, stmt, params)?;
    let stmt = prepared_statement.as_ref();
    if let Some(conflict) = stmt.on_conflict.as_ref() {
        super::conflict::validate_conflict_target(engine, &stmt.table, conflict)?;
    }
    let conflict_update_columns = if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        validate_mutation_columns(
            engine,
            &stmt.table,
            assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            "INSERT ON CONFLICT DO UPDATE",
        )?;
        Some(
            assignments
                .iter()
                .map(|assignment| assignment.column.clone())
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty() && !stmt.columns.is_empty() {
        validate_mutation_columns(
            engine,
            &stmt.table,
            stmt.columns.iter().map(String::as_str),
            "INSERT",
        )?;
    }
    super::conflict::validate_insert_returning(engine, stmt, params, inherited_ctes)?;
    uqa_sql::semantics::mutation_privileges::ensure_insert_target_privileges(
        engine,
        stmt,
        conflict_update_columns.as_deref(),
    )?;
    let view_original_query = !stmt.view_rule_relations.iter().try_fold(
        false,
        |suppressed, relation| -> Result<bool, SQLError> {
            Ok(suppressed
                || engine
                    .rules_for(relation, uqa_sql::ast::RuleEvent::Insert)?
                    .iter()
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
        },
    )?;
    let insert_original_query = view_original_query
        && !insert_rules
            .iter()
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    let has_before_insert_statement_trigger = insert_original_query
        && !engine
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
                false,
                &[],
            )?
            .is_empty();
    let has_before_update_statement_trigger =
        if let Some(columns) = conflict_update_columns.as_deref() {
            !engine
                .triggers_for(
                    &stmt.table,
                    uqa_sql::ast::TriggerTiming::Before,
                    uqa_sql::ast::TriggerEvent::Update,
                    false,
                    columns,
                )?
                .is_empty()
        } else {
            false
        };
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_insert_statement_trigger
            || has_before_update_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(
                engine.capture_statement_read_snapshot()?,
            ))
        }
        None => None,
    };
    if insert_original_query {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    if let Some(columns) = conflict_update_columns.as_deref() {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            columns,
        )?;
    }
    let snapshot_engine = statement_snapshot
        .as_deref()
        .map(|snapshot| engine.statement_read_snapshot_engine(snapshot));
    let read_engine = snapshot_engine.as_ref().unwrap_or(engine);
    let mut scope = crate::capabilities::query_scope::new_for_command(
        read_engine,
        stmt.statement_privilege_subject.as_deref(),
        stmt.relations_bound,
    )?;
    if let Some(parent) = inherited_ctes {
        scope.inherit_cte_bindings(parent);
    }
    scope.set_command_cte_snapshot(statement_snapshot.clone());
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut scope)?;
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    // Resolve the table's primary-key column name. Auto-increment (SERIAL / BIGSERIAL) wins; otherwise the scalar PRIMARY KEY column wins; otherwise use the conventional legacy `id` slot. Both VALUES and SELECT sources must derive the internal doc id from this same column or later primary-key rewrites can address a different row than the one that was inserted.
    let (auto_id_col, id_column, accepts_supplied_identity) =
        insert_identity_columns(engine, &stmt.table, "INSERT")?;
    let mut rule_source_rows = None;
    // INSERT ... SELECT: the query executor feeds each positional physical row directly into the INSERT sink. Ordinary source scans and scalar subqueries retain the statement snapshot, while a VOLATILE callback observes the logical mutations staged by preceding rows of this command.
    if let Some(source) = stmt.source.as_deref() {
        let surviving_view_rules_require_rows =
            crate::sql::rules::surviving_view_rules_require_event_rows(
                engine,
                &stmt.view_rule_relations,
                uqa_sql::ast::RuleEvent::Insert,
            )?;
        if !view_original_query && !surviving_view_rules_require_rows {
            rule_source_rows = Some(Vec::new());
        } else if !has_any_insert_rules {
            let snapshot_scope = scope.returning_statement_snapshot_scope();
            let mut source_scope = snapshot_scope.clone();
            source_scope.enable_command_progress_streaming();
            let consumer = Rc::new(InsertSelectConsumer::new(
                engine.insert_source_context(),
                stmt,
                params,
                snapshot_scope,
                InsertSelectIdentity {
                    auto_id_column: auto_id_col.clone(),
                    id_column: id_column.clone(),
                    accepts_supplied_identity,
                },
                conflict_update_columns.clone().unwrap_or_default(),
            )?);
            let overlay = MutationOverlayScope::new(engine);
            crate::sql::select::execute_query_plan_output(
                read_engine,
                source,
                params,
                &mut source_scope,
                crate::sql::select::QueryOutputMode::RowConsumer(consumer.clone()),
            )?;
            let PreparedInsertSelect {
                rows: prepared_rows,
                conflict_locks,
                affected,
                returning_rows,
                events,
                has_prepared_effect,
                has_prepared_auto_identity,
            } = consumer.take_prepared()?;
            drop(overlay);
            if has_prepared_effect || has_prepared_auto_identity {
                engine.prepare_explicit_transaction_writer()?;
                persist_auto_increment_identity(
                    engine,
                    &stmt.table,
                    auto_id_col.as_deref(),
                    "persist INSERT SELECT identity",
                )?;
            }
            drop(conflict_locks);
            let cancel = engine.cancellation_token();
            let apply_reader = prepared_rows
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            let mut publication = MutationPublicationBatch::default();
            for prepared_row in apply_reader {
                cancel.check()?;
                let prepared_row = prepared_row.map_err(crate::sql::select::physical_exec_error)?;
                let PreparedInsertSpillRow {
                    target_table,
                    document,
                    conflict: prepared,
                } = decode_prepared_insert_spill_row(prepared_row)?;
                apply_validated_prepared_insert(
                    engine,
                    &target_table,
                    document,
                    prepared,
                    false,
                    &mut publication,
                )?;
            }
            finish_mutation_publication(engine, &mut publication)?;
            fire_insert_after_triggers(
                engine,
                &stmt.table,
                insert_original_query,
                conflict_update_columns.as_deref(),
                &events,
            )?;
            if !stmt.returning.is_empty() {
                return dml_returning_result(
                    engine,
                    DmlReturningShape {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        aliases: &stmt.returning_aliases,
                        returning: &stmt.returning,
                        params,
                        ctes: &scope,
                        supplemental_schema: None,
                    },
                    returning_rows,
                    affected,
                );
            }
            return Ok(SQLResult::from_affected(affected));
        }
        if rule_source_rows.is_none() {
            let mut source = source.clone();
            if !view_original_query {
                if let Some(required_positions) =
                    required_view_rule_insert_input_positions(engine, stmt)?
                {
                    super::prune_unused_query_outputs(
                        &mut source,
                        &required_positions,
                        stmt.columns.len(),
                    );
                }
            }
            let mut source_scope = scope.returning_statement_snapshot_scope();
            source_scope.enable_command_progress_streaming();
            let result = crate::sql::select::execute_query_plan_with_ctes(
                read_engine,
                &source,
                params,
                &mut source_scope,
            )?;
            rule_source_rows = Some(insert_source_expression_rows(result)?);
        }
    }

    let implicit_columns = stmt.columns.is_empty();
    let columns: Vec<String> = if implicit_columns {
        // INSERT without explicit column list: project the table schema.
        engine
            .try_table_columns(&stmt.table)
            .map_err(|error| dml_storage_error("INSERT", error))?
    } else {
        stmt.columns.clone()
    };
    if view_original_query {
        validate_mutation_columns(
            engine,
            &stmt.table,
            columns.iter().map(String::as_str),
            "INSERT",
        )?;
    }

    // No explicit id and no auto-increment column: allocate a synthetic u64 doc_id at insert time. Every table has an implicit doc_id even when the schema declares no primary key.

    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    // Evaluate, validate, and stage every VALUES row before writer promotion. A scalar subquery inside VALUES may carry FOR UPDATE, so holding the backend writer during that wait would fabricate a deadlock. Ordinary subqueries retain the statement snapshot while VOLATILE functions read the logical overlay left by preceding rows, matching PostgreSQL 18 command visibility.
    let snapshot_scope = scope.returning_statement_snapshot_scope();
    let overlay = MutationOverlayScope::new(engine);
    let mut conflict_locks = InsertConflictLocks::new(&engine.referential_execution_context());
    let input_rows = rule_source_rows.as_deref().unwrap_or(&stmt.rows);
    let mut documents = Vec::with_capacity(input_rows.len());
    let mut target_tables = Vec::with_capacity(input_rows.len());
    let mut prepared_conflicts = Vec::with_capacity(input_rows.len());
    let mut events = super::MutationEventQueue::default();
    let mut has_prepared_effect = false;
    let mut has_prepared_auto_identity = false;
    let mut pending_rule_rows = Vec::with_capacity(input_rows.len());
    let required_rule_input_positions = (!view_original_query)
        .then(|| required_view_rule_insert_input_positions(engine, stmt))
        .transpose()?
        .flatten();
    for row in input_rows {
        cancel.check()?;
        if row.len() > columns.len() || (!implicit_columns && row.len() != columns.len()) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: if row.len() > columns.len() {
                    "INSERT has more expressions than target columns"
                } else {
                    "INSERT has more target columns than expressions"
                }
                .into(),
            });
        }
        let mut document = Document::new();
        for (i, col) in columns.iter().take(row.len()).enumerate() {
            if required_rule_input_positions
                .as_ref()
                .is_some_and(|required| !required.contains(&i))
            {
                continue;
            }
            let value = if has_any_insert_rules && matches!(row[i], super::ScalarExpr::Default) {
                None
            } else if !view_original_query {
                let value =
                    eval_mutation_expr(read_engine, &snapshot_scope, &row[i], None, params)?;
                match view_rule_insert_column_type(engine, stmt, i)? {
                    Some(ty) => Some(crate::sql::convert_value_to_column_type_with_engine(
                        engine, value, &ty,
                    )?),
                    None => Some(value),
                }
            } else {
                eval_mutation_assignment(
                    read_engine,
                    &snapshot_scope,
                    MutationAssignmentTarget {
                        table: &stmt.table,
                        column: col,
                        action: "INSERT",
                    },
                    &row[i],
                    None,
                    params,
                )?
            };
            if let Some(value) = value {
                document.insert(col.clone(), value);
            }
        }
        if has_any_insert_rules {
            pending_rule_rows.push(document);
            continue;
        }
        apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
        let prepared_auto_identity = prepare_auto_increment_identity(
            engine,
            &stmt.table,
            &id_column,
            auto_id_col.as_deref(),
            &mut document,
            "prepare INSERT identity",
        )?;
        has_prepared_auto_identity |= prepared_auto_identity.is_some();
        let target_table = partition_insert_target(
            engine,
            &stmt.table,
            &document,
            params,
            stmt.include_descendants,
        )?;
        engine.lock_relation(
            &target_table,
            crate::row_locks::RelationLockMode::RowExclusive,
        )?;
        let insert_identity = match prepared_auto_identity {
            Some(identity) => identity,
            None => prepare_insert_identity(
                engine,
                &target_table,
                &id_column,
                accepts_supplied_identity,
                None,
                &mut document,
                "prepare INSERT identity",
            )?,
        };
        if let Some(staged) = prepare_values_insert_row(
            engine.mutation_preparation_context(),
            stmt,
            params,
            &snapshot_scope,
            conflict_update_columns.as_deref().unwrap_or(&[]),
            auto_id_col.as_deref(),
            &id_column,
            accepts_supplied_identity,
            target_table,
            document,
            insert_identity,
            &mut conflict_locks,
            events.referential_actions_mut(),
        )? {
            if let Some(returning) = staged.returning {
                returning_rows.push(returning);
            }
            events.append_after_rows(staged.after_row_events);
            if staged.prepared_effect {
                affected += 1;
                has_prepared_effect = true;
            }
            documents.push(staged.document);
            target_tables.push(staged.target_table);
            prepared_conflicts.push(staged.prepared);
        }
    }
    let mut view_rule_rows = Vec::with_capacity(pending_rule_rows.len());
    for document in &pending_rule_rows {
        let rule_doc_id = document_supplied_id(
            document,
            &id_column,
            auto_id_col.as_deref() == Some(id_column.as_str()),
        )?;
        view_rule_rows.push(crate::sql::rules::RuleRowImage {
            old_storage_table: None,
            old_doc_id: None,
            old: None,
            new_storage_table: None,
            new_doc_id: rule_doc_id,
            new: Some(document.clone()),
            context: None,
        });
    }
    let view_rule_batches = super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
        context: engine.view_rule_execution_context(),
        relations: &stmt.view_rule_relations,
        event: uqa_sql::ast::RuleEvent::Insert,
        rows: &view_rule_rows,
        params,
        scope: &snapshot_scope,
        insert_plans: &stmt.view_rule_insert_plans,
        update_plans: &[],
        document_relation: None,
    })?;
    let mut pending_base_rows = Vec::with_capacity(pending_rule_rows.len());
    for (index, mut document) in pending_rule_rows.into_iter().enumerate() {
        if view_rule_batches.suppresses(index) {
            continue;
        }
        apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
        let prepared_auto_identity = prepare_auto_increment_identity(
            engine,
            &stmt.table,
            &id_column,
            auto_id_col.as_deref(),
            &mut document,
            "prepare INSERT identity",
        )?;
        has_prepared_auto_identity |= prepared_auto_identity.is_some();
        pending_base_rows.push((document, prepared_auto_identity));
    }
    let rule_batch = (has_insert_rules && view_original_query)
        .then(|| {
            let rule_rows = pending_base_rows
                .iter()
                .map(|(document, _)| {
                    let mut rule_document = document.clone();
                    crate::sql::generated::refresh_stored_generated_columns(
                        engine,
                        &stmt.table,
                        &mut rule_document,
                    )?;
                    let rule_doc_id = document_supplied_id(
                        &rule_document,
                        &id_column,
                        auto_id_col.as_deref() == Some(id_column.as_str()),
                    )?;
                    Ok(crate::sql::rules::RuleRowImage {
                        old_storage_table: None,
                        old_doc_id: None,
                        old: None,
                        new_storage_table: None,
                        new_doc_id: rule_doc_id,
                        new: Some(rule_document),
                        context: None,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            crate::sql::rules::prepare_rule_batch(
                engine,
                &stmt.table,
                uqa_sql::ast::RuleEvent::Insert,
                rule_rows,
            )
        })
        .transpose()?;
    if has_any_insert_rules {
        for (rule_index, (mut document, prepared_auto_identity)) in
            pending_base_rows.into_iter().enumerate()
        {
            if rule_batch
                .as_ref()
                .is_some_and(|rule_batch| rule_batch.suppresses(rule_index))
            {
                continue;
            }
            let target_table = partition_insert_target(
                engine,
                &stmt.table,
                &document,
                params,
                stmt.include_descendants,
            )?;
            engine.lock_relation(
                &target_table,
                crate::row_locks::RelationLockMode::RowExclusive,
            )?;
            let insert_identity = match prepared_auto_identity {
                Some(identity) => identity,
                None => prepare_insert_identity(
                    engine,
                    &target_table,
                    &id_column,
                    accepts_supplied_identity,
                    None,
                    &mut document,
                    "prepare INSERT identity",
                )?,
            };
            let Some(staged) = prepare_values_insert_row(
                engine.mutation_preparation_context(),
                stmt,
                params,
                &snapshot_scope,
                conflict_update_columns.as_deref().unwrap_or(&[]),
                auto_id_col.as_deref(),
                &id_column,
                accepts_supplied_identity,
                target_table,
                document,
                insert_identity,
                &mut conflict_locks,
                events.referential_actions_mut(),
            )?
            else {
                continue;
            };
            if let Some(returning) = staged.returning {
                returning_rows.push(returning);
            }
            events.append_after_rows(staged.after_row_events);
            if staged.prepared_effect {
                affected += 1;
                has_prepared_effect = true;
            }
            documents.push(staged.document);
            target_tables.push(staged.target_table);
            prepared_conflicts.push(staged.prepared);
        }
    }
    drop(overlay);
    if has_prepared_effect || has_prepared_auto_identity {
        engine.prepare_explicit_transaction_writer()?;
        persist_auto_increment_identity(
            engine,
            &stmt.table,
            auto_id_col.as_deref(),
            "persist INSERT identity",
        )?;
    }
    drop(conflict_locks);
    let mut publication = MutationPublicationBatch::default();
    for ((target_table, document), prepared) in target_tables
        .into_iter()
        .zip(documents)
        .zip(prepared_conflicts)
    {
        cancel.check()?;
        let supplied = matches!(
            &prepared,
            PreparedInsertConflict::Insert { supplied: true, .. }
        );
        let id_column_is_unique_key = engine
            .try_unique_columns(&target_table)
            .map_err(|err| dml_storage_error("INSERT", err))?
            .contains(&id_column);
        let known_new = stmt.on_conflict.is_none() && (!supplied || id_column_is_unique_key);
        let document = Arc::try_unwrap(document).map_err(|_| {
            SQLError::Internal("INSERT command overlay retained a staged document".into())
        })?;
        apply_validated_prepared_insert(
            engine,
            &target_table,
            document,
            prepared,
            known_new,
            &mut publication,
        )?;
    }
    finish_mutation_publication(engine, &mut publication)?;
    fire_insert_after_triggers(
        engine,
        &stmt.table,
        insert_original_query,
        conflict_update_columns.as_deref(),
        &events,
    )?;
    let (rule_returning, rule_affected, rule_sets_command_tag) =
        if let Some(rule_batch) = rule_batch.as_ref() {
            let outcome = rule_batch.execute_actions_with_affected(
                engine.rule_execution_context(),
                crate::sql::rules::RuleReturningRequest::from_plan(
                    &stmt.returning,
                    &stmt.returning_aliases,
                    &stmt.subqueries,
                ),
            )?;
            (
                outcome.returning,
                outcome.affected_rows,
                outcome.sets_command_tag,
            )
        } else {
            (None, 0, false)
        };
    let view_rule_outcome = view_rule_batches.execute_actions_with_affected(
        engine.rule_execution_context(),
        stmt.view_rule_returning.as_ref(),
    )?;
    let view_rule_returning = view_rule_outcome.returning;
    if view_rule_returning.is_some() && rule_returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if !stmt.returning.is_empty() {
        if let Some(view_rule_returning) = view_rule_returning {
            return view_rule_returning.project(
                engine.returning_execution_context(),
                params,
                &scope,
                None,
            );
        }
        let shape = DmlReturningShape {
            table: &stmt.table,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &scope,
            supplemental_schema: None,
        };
        if let Some(rule_returning) = rule_returning {
            return rule_returning.project(engine.returning_execution_context(), shape);
        }
        return dml_returning_result(engine, shape, returning_rows, affected);
    }
    let rule_affected = if view_rule_outcome.sets_command_tag {
        view_rule_outcome.affected_rows
    } else if rule_sets_command_tag {
        rule_affected
    } else {
        0
    };
    Ok(SQLResult::from_affected(
        if affected == 0 && !insert_original_query {
            rule_affected
        } else {
            affected
        },
    ))
}

fn fire_insert_after_triggers(
    engine: &Engine,
    table: &str,
    insert_original_query: bool,
    conflict_update_columns: Option<&[String]>,
    events: &super::MutationEventQueue,
) -> Result<(), SQLError> {
    uqa_execution::mutation::insert::triggers::fire_insert_after_triggers(
        &engine.trigger_execution_context(),
        table,
        insert_original_query,
        conflict_update_columns,
        events,
    )
}
