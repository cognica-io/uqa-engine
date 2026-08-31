//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use crate::sql::select::physical_work_mem_bytes;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::{
    build_returning_row, coerce_to_column_type, dml_returning_result, dml_storage_error,
    document_supplied_id, eval_lowered_expression, eval_mutation_assignment, eval_mutation_expr,
    finish_mutation_publication, insert_identity_columns, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, partition_insert_target,
    persist_auto_increment_identity, prepare_auto_increment_identity, prepare_insert_identity,
    publish_prepared_mutation_action, stage_prepared_document_rewrite,
    validate_document_non_key_constraints, validate_key_constraints, validate_mutation_columns,
    validate_returning_alias_relations, validate_view_checks, BTreeSet, ColumnType,
    ConflictActionPlan, ConflictPlan, CteScope, DmlReturningShape, DocId, Document, Engine,
    InsertConflictLocks, InsertConflictPreparation, InsertPlan, MutationAssignmentTarget,
    MutationOverlayScope, MutationPublicationBatch, MutationRowImage, MutationRowImages,
    PreparedDocumentInsert, PreparedInsertConflict, PreparedMutationAction, ReturningProjectionRow,
    SQLError, SQLParam, SQLResult, ViewCheckContext,
};

mod codec;
mod staging;

use codec::{
    decode_prepared_insert_spill_row, encode_prepared_insert_spill_row,
    prepared_insert_spill_schema, PreparedInsertSpillRow,
};

pub(in crate::sql) use staging::{
    apply_missing_column_defaults, refresh_insert_identity_after_trigger,
};
use staging::{
    apply_validated_prepared_insert, attach_prepared_insert_identity, prepare_values_insert_row,
    stage_prepared_insert_row,
};

struct InsertSelectConsumer {
    state: RefCell<InsertSelectConsumerState>,
}

struct InsertSelectIdentity {
    auto_id_column: Option<String>,
    id_column: String,
    accepts_supplied_identity: bool,
}

struct InsertSelectConsumerState {
    stmt: InsertPlan,
    params: Vec<SQLParam>,
    snapshot_scope: CteScope,
    auto_id_column: Option<String>,
    id_column: String,
    accepts_supplied_identity: bool,
    conflict_update_columns: Vec<String>,
    columns: Option<Vec<String>>,
    result_width: Option<usize>,
    prepared_schema: uqa_execution::RowSchema,
    prepared_buffer: Option<uqa_execution::SpillBuffer>,
    conflict_locks: Option<InsertConflictLocks>,
    affected: u64,
    returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    events: super::MutationEventQueue,
    has_prepared_effect: bool,
    has_prepared_auto_identity: bool,
}

struct PreparedInsertSelect {
    rows: uqa_execution::SharedSpill,
    conflict_locks: InsertConflictLocks,
    affected: u64,
    returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    events: super::MutationEventQueue,
    has_prepared_effect: bool,
    has_prepared_auto_identity: bool,
}

struct PreparedInsertRowContext<'a> {
    engine: &'a Engine,
    stmt: &'a InsertPlan,
    storage_table: &'a str,
    document: &'a Document,
    shared_document: Option<&'a Arc<Document>>,
    conflict_update_columns: &'a [String],
    params: &'a [SQLParam],
    scope: &'a CteScope,
}

impl InsertSelectConsumer {
    fn new(
        engine: &Engine,
        stmt: &InsertPlan,
        params: &[SQLParam],
        snapshot_scope: CteScope,
        identity: InsertSelectIdentity,
        conflict_update_columns: Vec<String>,
    ) -> Result<Self, SQLError> {
        let InsertSelectIdentity {
            auto_id_column,
            id_column,
            accepts_supplied_identity,
        } = identity;
        let prepared_schema = prepared_insert_spill_schema();
        Ok(Self {
            state: RefCell::new(InsertSelectConsumerState {
                stmt: stmt.clone(),
                params: params.to_vec(),
                snapshot_scope,
                auto_id_column,
                id_column,
                accepts_supplied_identity,
                conflict_update_columns,
                columns: None,
                result_width: None,
                prepared_schema,
                prepared_buffer: Some(uqa_execution::SpillBuffer::new(
                    physical_work_mem_bytes(engine.query_runtime_view())?.max(1),
                )),
                conflict_locks: Some(InsertConflictLocks::new(engine)),
                affected: 0,
                returning_rows: Vec::new(),
                events: super::MutationEventQueue::default(),
                has_prepared_effect: false,
                has_prepared_auto_identity: false,
            }),
        })
    }

    fn take_prepared(&self) -> Result<PreparedInsertSelect, SQLError> {
        let mut state = self.state.borrow_mut();
        let buffer = state.prepared_buffer.take().ok_or_else(|| {
            SQLError::Internal("INSERT SELECT row consumer was finalized more than once".into())
        })?;
        let rows = buffer
            .into_shared(state.prepared_schema.clone())
            .map_err(crate::sql::select::physical_exec_error)?;
        let conflict_locks = state.conflict_locks.take().ok_or_else(|| {
            SQLError::Internal("INSERT SELECT conflict locks were finalized more than once".into())
        })?;
        Ok(PreparedInsertSelect {
            rows,
            conflict_locks,
            affected: state.affected,
            returning_rows: std::mem::take(&mut state.returning_rows),
            events: std::mem::take(&mut state.events),
            has_prepared_effect: state.has_prepared_effect,
            has_prepared_auto_identity: state.has_prepared_auto_identity,
        })
    }
}

impl crate::sql::select::QueryRowConsumer for InsertSelectConsumer {
    fn begin(
        &self,
        engine: &Engine,
        source_columns: &[String],
        _schema: &uqa_execution::RowSchema,
    ) -> Result<(), SQLError> {
        let mut state = self.state.borrow_mut();
        let result_width = source_columns.len();
        let implicit_columns = state.stmt.columns.is_empty();
        let columns = if implicit_columns {
            let target_columns = engine
                .try_table_columns(&state.stmt.table)
                .map_err(|error| dml_storage_error("INSERT SELECT", error))?;
            if target_columns.is_empty() {
                source_columns.to_vec()
            } else {
                target_columns
            }
        } else {
            state.stmt.columns.clone()
        };
        validate_mutation_columns(
            engine,
            &state.stmt.table,
            columns.iter().map(String::as_str),
            "INSERT SELECT",
        )?;
        if result_width > columns.len() || (!implicit_columns && result_width != columns.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "INSERT SELECT width {result_width} != column count {}",
                columns.len()
            )));
        }
        if let (Some(existing_columns), Some(existing_width)) =
            (state.columns.as_ref(), state.result_width)
        {
            if existing_columns != &columns || existing_width != result_width {
                return Err(SQLError::Internal(
                    "INSERT SELECT row consumer was rebound to a different source shape".into(),
                ));
            }
            return Ok(());
        }
        state.columns = Some(columns);
        state.result_width = Some(result_width);
        Ok(())
    }

    fn consume(
        &self,
        engine: &Engine,
        source_row: uqa_execution::OwnedPhysicalRow,
    ) -> Result<crate::sql::select::QueryConsumerControl, SQLError> {
        engine.cancellation_token().check()?;
        let mut state = self.state.borrow_mut();
        let InsertSelectConsumerState {
            stmt,
            params,
            snapshot_scope,
            auto_id_column,
            id_column,
            accepts_supplied_identity,
            conflict_update_columns,
            columns,
            result_width,
            prepared_schema,
            prepared_buffer,
            conflict_locks,
            affected,
            returning_rows,
            events,
            has_prepared_effect,
            has_prepared_auto_identity,
        } = &mut *state;
        let columns = columns.as_ref().ok_or_else(|| {
            SQLError::Internal("INSERT SELECT row consumer was not initialized".into())
        })?;
        let result_width = result_width.ok_or_else(|| {
            SQLError::Internal("INSERT SELECT row consumer has no source width".into())
        })?;
        let source_row = source_row.view();
        let mut document = Document::new();
        for (index, column) in columns.iter().take(result_width).enumerate() {
            if crate::sql::generated::generated_column_kind(engine, &stmt.table, column)?.is_some()
            {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{column}` is a generated column; only DEFAULT may be assigned"
                )));
            }
            let value = source_row
                .value_at(index)
                .cloned()
                .unwrap_or(super::Value::Null);
            document.insert(
                column.clone(),
                coerce_to_column_type(engine, &stmt.table, column, value)?,
            );
        }
        apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
        let prepared_auto_identity = prepare_auto_increment_identity(
            engine,
            &stmt.table,
            id_column,
            auto_id_column.as_deref(),
            &mut document,
            "prepare INSERT SELECT identity",
        )?;
        *has_prepared_auto_identity |= prepared_auto_identity.is_some();
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
        let mut insert_identity = match prepared_auto_identity {
            Some(identity) => identity,
            None => prepare_insert_identity(
                engine,
                &target_table,
                id_column,
                *accepts_supplied_identity,
                None,
                &mut document,
                "prepare INSERT SELECT identity",
            )?,
        };
        let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
            engine,
            &target_table,
            uqa_sql::ast::TriggerEvent::Insert,
            insert_identity.0,
            None,
            Some(&document),
            &[],
        )?
        else {
            return Ok(crate::sql::select::QueryConsumerControl::Continue);
        };
        document = triggered_document;
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &target_table,
            &mut document,
        )?;
        refresh_insert_identity_after_trigger(
            engine,
            &target_table,
            id_column,
            *accepts_supplied_identity,
            auto_id_column.as_deref(),
            &document,
            &mut insert_identity,
        )?;
        let trigger_target = partition_insert_target(
            engine,
            &stmt.table,
            &document,
            params,
            stmt.include_descendants,
        )?;
        if trigger_target != target_table {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "moving row to another partition during a BEFORE FOR EACH ROW trigger is not supported".into(),
            });
        }
        super::stamp_tuple_xmin(engine, &target_table, &mut document)?;
        lock_existing_document_foreign_key_dependencies(engine, &target_table, &document)?;
        let prepared_conflict = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            conflict_locks
                .as_mut()
                .ok_or_else(|| {
                    SQLError::Internal("INSERT SELECT conflict locks are unavailable".into())
                })?
                .prepare_document(
                    InsertConflictPreparation {
                        engine,
                        table: &target_table,
                        target_qualifier: &stmt.target_qualifier,
                        on_conflict,
                        document: &document,
                        params,
                        scope: snapshot_scope,
                    },
                    events.referential_actions_mut(),
                )?
        } else {
            let _key_locks =
                lock_document_key_dependencies(engine, &target_table, &document, None)?;
            PreparedInsertConflict::Unresolved
        };
        let mut prepared_conflict =
            attach_prepared_insert_identity(prepared_conflict, insert_identity);
        let prepared_effect = !matches!(&prepared_conflict, PreparedInsertConflict::Skip);
        let (returning, row_after_events) = stage_prepared_insert_row(
            PreparedInsertRowContext {
                engine,
                stmt,
                storage_table: &target_table,
                document: &document,
                shared_document: None,
                conflict_update_columns,
                params,
                scope: snapshot_scope,
            },
            &mut prepared_conflict,
        )?;
        if let Some(row) = returning {
            returning_rows.push(row);
        }
        events.append_after_rows(row_after_events);
        prepared_buffer
            .as_mut()
            .ok_or_else(|| {
                SQLError::Internal("INSERT SELECT prepared buffer is unavailable".into())
            })?
            .push(uqa_execution::Batch::from_physical_rows(
                prepared_schema.clone(),
                vec![encode_prepared_insert_spill_row(PreparedInsertSpillRow {
                    target_table,
                    document,
                    conflict: prepared_conflict,
                })],
            ))
            .map_err(crate::sql::select::physical_exec_error)?;
        if prepared_effect {
            *affected += 1;
            *has_prepared_effect = true;
        }
        Ok(crate::sql::select::QueryConsumerControl::Continue)
    }
}

pub(in crate::sql) fn run_insert(
    engine: &Engine,
    stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    super::run_mutation_command(engine, move |engine| {
        run_insert_inner(engine, &stmt, params)
    })
}

fn insert_source_expression_rows(
    result: SQLResult,
) -> Result<Vec<Vec<uqa_execution::ScalarExpr>>, SQLError> {
    let values = match result.positional_rows {
        Some(rows) => rows,
        None => result
            .rows
            .into_iter()
            .map(|row| {
                result
                    .columns
                    .iter()
                    .map(|column| {
                        row.get(column).cloned().ok_or_else(|| {
                            SQLError::Internal(format!(
                                "INSERT SELECT result omitted output column `{column}`"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, SQLError>>()
            })
            .collect::<Result<Vec<_>, SQLError>>()?,
    };
    Ok(values
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(uqa_execution::ScalarExpr::Literal)
                .collect()
        })
        .collect())
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            return super::view_triggers::run_view_insert_inner(engine, stmt, params);
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
            return super::view_triggers::run_view_insert_inner(engine, stmt, params);
        }
        let rewritten = super::view_automatic::rewrite_insert_to_base(engine, stmt, params)?;
        return run_insert_inner(engine, &rewritten, params);
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
    let statement_snapshot = (has_before_insert_statement_trigger
        || has_before_update_statement_trigger)
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
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
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut scope = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut scope)?;
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
                engine,
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
        let cols = engine
            .try_table_columns(&stmt.table)
            .map_err(|error| dml_storage_error("INSERT", error))?;
        if cols.is_empty() {
            return Err(SQLError::Unsupported(
                "INSERT without column list against a table with no schema".into(),
            ));
        }
        cols
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
    let mut conflict_locks = InsertConflictLocks::new(engine);
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
            return Err(SQLError::TypeMismatch(format!(
                "row width {} != column count {}",
                row.len(),
                columns.len()
            )));
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
                    Some(ty) => Some(crate::sql::convert_value_to_column_type(value, &ty)?),
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
            engine,
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
        engine,
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
                engine,
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
    let (rule_returning, rule_affected, rule_executed) =
        if let Some(rule_batch) = rule_batch.as_ref() {
            let outcome = rule_batch.execute_actions_with_affected(
                engine,
                crate::sql::rules::RuleReturningRequest::from_plan(
                    &stmt.returning,
                    &stmt.returning_aliases,
                    &stmt.subqueries,
                ),
            )?;
            (
                outcome.returning,
                outcome.affected_rows,
                outcome.executed_action,
            )
        } else {
            (None, 0, false)
        };
    let view_rule_outcome = view_rule_batches
        .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
    let view_rule_returning = view_rule_outcome.returning;
    if view_rule_returning.is_some() && rule_returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if !stmt.returning.is_empty() {
        if let Some(view_rule_returning) = view_rule_returning {
            return view_rule_returning.project(engine, params, &scope, None);
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
            return rule_returning.project(engine, shape);
        }
        return dml_returning_result(engine, shape, returning_rows, affected);
    }
    let rule_affected = if view_rule_outcome.executed_action {
        view_rule_outcome.affected_rows
    } else if rule_executed {
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

fn view_rule_insert_column_type(
    engine: &Engine,
    stmt: &InsertPlan,
    input_position: usize,
) -> Result<Option<ColumnType>, SQLError> {
    for plan in &stmt.view_rule_insert_plans {
        let Some(column) = plan.supplied_columns.get(input_position) else {
            continue;
        };
        let definition = engine
            .view_definition(&plan.relation)?
            .ok_or_else(|| SQLError::UnknownTable(plan.relation.clone()))?;
        let schema = engine.stored_view_schema(&definition)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    public.eq_ignore_ascii_case(column).then_some(position)
                })
        else {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                plan.relation, column
            )));
        };
        return Ok(schema.column_type(position).cloned());
    }
    Ok(None)
}

fn required_view_rule_insert_input_positions(
    engine: &Engine,
    stmt: &InsertPlan,
) -> Result<Option<BTreeSet<usize>>, SQLError> {
    let mut required = BTreeSet::new();
    for plan in &stmt.view_rule_insert_plans {
        let Some(columns) = crate::sql::rules::relation_rule_row_columns(
            engine,
            &plan.relation,
            uqa_sql::ast::RuleEvent::Insert,
        )?
        else {
            return Ok(None);
        };
        required.extend(
            plan.supplied_columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| columns.contains(column).then_some(position)),
        );
        if crate::sql::rules::relation_suppresses_original_query(
            engine,
            &plan.relation,
            uqa_sql::ast::RuleEvent::Insert,
        )? {
            break;
        }
    }
    Ok(Some(required))
}

fn fire_insert_after_triggers(
    engine: &Engine,
    table: &str,
    insert_original_query: bool,
    conflict_update_columns: Option<&[String]>,
    events: &super::MutationEventQueue,
) -> Result<(), SQLError> {
    let insert_transition = if insert_original_query {
        crate::sql::triggers::build_transition_tables(
            engine,
            table,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let update_transition = if let Some(columns) = conflict_update_columns {
        crate::sql::triggers::build_transition_tables(
            engine,
            table,
            uqa_sql::ast::TriggerEvent::Update,
            columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(engine)?;
    let mut transition_tables = insert_transition
        .iter()
        .chain(update_transition.iter())
        .collect::<Vec<_>>();
    transition_tables.extend(referential_transition.iter());
    let mut root_events = Vec::new();
    if conflict_update_columns.is_some() {
        root_events.push(uqa_sql::ast::TriggerEvent::Update);
    }
    if insert_original_query {
        root_events.push(uqa_sql::ast::TriggerEvent::Insert);
    }
    for generation in crate::sql::triggers::after_trigger_generations(&transition_tables) {
        crate::sql::triggers::fire_after_row_trigger_events_for_generation(
            engine,
            events.after_rows(),
            &transition_tables,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            engine,
            &referential_transition,
            table,
            &root_events,
            generation,
        )?;
        if let Some(columns) = conflict_update_columns {
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
                table,
                uqa_sql::ast::TriggerEvent::Update,
                columns,
                &update_transition,
                generation,
            )?;
        }
        if insert_original_query {
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
                table,
                uqa_sql::ast::TriggerEvent::Insert,
                &[],
                &insert_transition,
                generation,
            )?;
        }
    }
    Ok(())
}
