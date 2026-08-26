//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::{
    apply_validated_prepared_document_rewrite, build_returning_row, coerce_to_column_type,
    decode_prepared_insert_conflict, dml_returning_result, dml_storage_error, document_supplied_id,
    encode_prepared_insert_conflict, eval_lowered_expression, eval_mutation_assignment,
    index_vectors_for_type, insert_identity_columns, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, partition_insert_target,
    persist_auto_increment_identity, prepare_auto_increment_identity, prepare_insert_identity,
    stage_prepared_document_rewrite, validate_document_non_key_constraints,
    validate_key_constraints, validate_mutation_columns, validate_returning_alias_relations,
    BTreeMap, ColumnType, ConflictActionPlan, ConflictPlan, CteScope, DmlCommandMutationOverlay,
    DmlReturningShape, DocId, Document, Engine, InsertConflictLocks, InsertConflictPreparation,
    InsertPlan, MutationAssignmentTarget, PreparedInsertConflict, ReturningProjectionRow,
    ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult,
};

struct InsertSelectConsumer {
    state: RefCell<InsertSelectConsumerState>,
}

struct InsertSelectConsumerState {
    stmt: InsertPlan,
    params: Vec<SQLParam>,
    snapshot_scope: CteScope,
    auto_id_column: Option<String>,
    id_column: String,
    conflict_update_columns: Vec<String>,
    columns: Option<Vec<String>>,
    result_width: Option<usize>,
    prepared_schema: uqa_execution::RowSchema,
    prepared_buffer: Option<uqa_execution::SpillBuffer>,
    conflict_locks: Option<InsertConflictLocks>,
    affected: u64,
    returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    after_row_events: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    referential_actions: super::ReferentialActionContext,
    has_prepared_effect: bool,
    has_prepared_auto_identity: bool,
}

struct PreparedInsertSelect {
    rows: uqa_execution::SharedSpill,
    conflict_locks: InsertConflictLocks,
    affected: u64,
    returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    after_row_events: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    referential_actions: super::ReferentialActionContext,
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
        auto_id_column: Option<String>,
        id_column: String,
        conflict_update_columns: Vec<String>,
    ) -> Result<Self, SQLError> {
        let prepared_schema = uqa_execution::RowSchema::new(vec![
            "__uqa_insert_table".into(),
            "__uqa_insert_document".into(),
            "__uqa_insert_conflict".into(),
        ]);
        Ok(Self {
            state: RefCell::new(InsertSelectConsumerState {
                stmt: stmt.clone(),
                params: params.to_vec(),
                snapshot_scope,
                auto_id_column,
                id_column,
                conflict_update_columns,
                columns: None,
                result_width: None,
                prepared_schema,
                prepared_buffer: Some(uqa_execution::SpillBuffer::new(
                    crate::sql::select::physical_work_mem_bytes(engine)?.max(1),
                )),
                conflict_locks: Some(InsertConflictLocks::new(engine)),
                affected: 0,
                returning_rows: Vec::new(),
                after_row_events: Vec::new(),
                referential_actions: super::ReferentialActionContext::default(),
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
            after_row_events: std::mem::take(&mut state.after_row_events),
            referential_actions: std::mem::take(&mut state.referential_actions),
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
            conflict_update_columns,
            columns,
            result_width,
            prepared_schema,
            prepared_buffer,
            conflict_locks,
            affected,
            returning_rows,
            after_row_events,
            referential_actions,
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
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &stmt.table,
            &mut document,
        )?;
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
        refresh_insert_identity_after_trigger(
            engine,
            &target_table,
            id_column,
            auto_id_column.as_deref(),
            &document,
            &mut insert_identity,
        )?;
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &target_table,
            &mut document,
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
                    referential_actions,
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
        after_row_events.extend(row_after_events);
        prepared_buffer
            .as_mut()
            .ok_or_else(|| {
                SQLError::Internal("INSERT SELECT prepared buffer is unavailable".into())
            })?
            .push(uqa_execution::Batch::from_physical_rows(
                prepared_schema.clone(),
                vec![uqa_execution::PhysicalRow::from_values(vec![
                    super::Value::Str(target_table),
                    super::Value::Map(document),
                    encode_prepared_insert_conflict(prepared_conflict),
                ])],
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
    if engine.transaction_depth() != 0 {
        run_insert_inner(engine, &stmt, params)
    } else {
        engine.transaction(move |engine| run_insert_inner(engine, &stmt, params))
    }
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    let mut scope = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut scope)?;
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &stmt.table,
        uqa_sql::ast::TriggerTiming::Before,
        uqa_sql::ast::TriggerEvent::Insert,
        &[],
    )?;
    if let Some(columns) = conflict_update_columns.as_deref() {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            columns,
        )?;
    }
    // Resolve the table's primary-key column name. Auto-increment
    // (SERIAL / BIGSERIAL) wins; otherwise the scalar PRIMARY KEY
    // column wins; otherwise use the conventional legacy `id` slot.
    // Both VALUES and SELECT sources must derive the internal doc id
    // from this same column or later primary-key rewrites can address a
    // different row than the one that was inserted.
    let (auto_id_col, id_column) = insert_identity_columns(engine, &stmt.table, "INSERT")?;
    // INSERT ... SELECT: the query executor feeds each positional physical row directly into the INSERT sink. Ordinary source scans and scalar subqueries retain the statement snapshot, while a VOLATILE callback observes the logical mutations staged by preceding rows of this command.
    if let Some(source) = stmt.source.as_deref() {
        let snapshot_scope = scope.returning_statement_snapshot_scope();
        let mut source_scope = snapshot_scope.clone();
        source_scope.enable_command_progress_streaming();
        let consumer = Rc::new(InsertSelectConsumer::new(
            engine,
            stmt,
            params,
            snapshot_scope,
            auto_id_col.clone(),
            id_column.clone(),
            conflict_update_columns.clone().unwrap_or_default(),
        )?);
        let overlay = DmlCommandMutationOverlay::new(engine);
        crate::sql::select::execute_query_plan_output(
            engine,
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
            after_row_events,
            referential_actions,
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
        for prepared_row in apply_reader {
            cancel.check()?;
            let prepared_row = prepared_row.map_err(crate::sql::select::physical_exec_error)?;
            let Some(super::Value::Str(target_table)) = prepared_row.view().value_at(0).cloned()
            else {
                return Err(SQLError::Internal(
                    "INSERT SELECT prepared spill lost its table payload".into(),
                ));
            };
            let Some(super::Value::Map(document)) = prepared_row.view().value_at(1).cloned() else {
                return Err(SQLError::Internal(
                    "INSERT SELECT prepared spill lost its document payload".into(),
                ));
            };
            let mut prepared = decode_prepared_insert_conflict(
                prepared_row.view().value_at(2).cloned().ok_or_else(|| {
                    SQLError::Internal(
                        "INSERT SELECT prepared spill lost its conflict payload".into(),
                    )
                })?,
            )?;
            apply_validated_prepared_insert(engine, &target_table, document, &mut prepared, false)?;
        }
        crate::sql::triggers::fire_after_row_trigger_events(engine, &after_row_events)?;
        referential_actions.fire_after_statement_triggers(engine)?;
        if let Some(columns) = conflict_update_columns.as_deref() {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &stmt.table,
                uqa_sql::ast::TriggerTiming::After,
                uqa_sql::ast::TriggerEvent::Update,
                columns,
            )?;
        }
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
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
    validate_mutation_columns(
        engine,
        &stmt.table,
        columns.iter().map(String::as_str),
        "INSERT",
    )?;

    // No explicit id and no auto-increment column: allocate a synthetic
    // u64 doc_id at insert time. Every table has an implicit doc_id even when the
    // schema declares no primary key.

    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    // Evaluate, validate, and stage every VALUES row before writer promotion. A scalar subquery inside VALUES may carry FOR UPDATE, so holding the backend writer during that wait would fabricate a deadlock. Ordinary subqueries retain the statement snapshot while VOLATILE functions read the logical overlay left by preceding rows, matching PostgreSQL 18 command visibility.
    let snapshot_scope = scope.returning_statement_snapshot_scope();
    let overlay = DmlCommandMutationOverlay::new(engine);
    let mut conflict_locks = InsertConflictLocks::new(engine);
    let mut documents = Vec::with_capacity(stmt.rows.len());
    let mut target_tables = Vec::with_capacity(stmt.rows.len());
    let mut prepared_conflicts = Vec::with_capacity(stmt.rows.len());
    let mut after_row_events = Vec::with_capacity(stmt.rows.len());
    let mut referential_actions = super::ReferentialActionContext::default();
    let mut has_prepared_effect = false;
    for row in &stmt.rows {
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
            if let Some(value) = eval_mutation_assignment(
                engine,
                &snapshot_scope,
                MutationAssignmentTarget {
                    table: &stmt.table,
                    column: col,
                    action: "INSERT",
                },
                &row[i],
                None,
                params,
            )? {
                document.insert(col.clone(), value);
            }
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
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &stmt.table,
            &mut document,
        )?;
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
                &id_column,
                None,
                &mut document,
                "prepare INSERT identity",
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
            continue;
        };
        document = triggered_document;
        refresh_insert_identity_after_trigger(
            engine,
            &target_table,
            &id_column,
            auto_id_col.as_deref(),
            &document,
            &mut insert_identity,
        )?;
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &target_table,
            &mut document,
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
        lock_existing_document_foreign_key_dependencies(engine, &target_table, &document)?;
        let prepared = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            conflict_locks.prepare_document(
                InsertConflictPreparation {
                    engine,
                    table: &target_table,
                    target_qualifier: &stmt.target_qualifier,
                    on_conflict,
                    document: &document,
                    params,
                    scope: &snapshot_scope,
                },
                &mut referential_actions,
            )?
        } else {
            let _key_locks =
                lock_document_key_dependencies(engine, &target_table, &document, None)?;
            PreparedInsertConflict::Unresolved
        };
        let mut prepared = attach_prepared_insert_identity(prepared, insert_identity);
        let prepared_effect = !matches!(&prepared, PreparedInsertConflict::Skip);
        let document = Arc::new(document);
        let (returning, row_after_events) = stage_prepared_insert_row(
            PreparedInsertRowContext {
                engine,
                stmt,
                storage_table: &target_table,
                document: document.as_ref(),
                shared_document: Some(&document),
                conflict_update_columns: conflict_update_columns.as_deref().unwrap_or(&[]),
                params,
                scope: &snapshot_scope,
            },
            &mut prepared,
        )?;
        if let Some(returning) = returning {
            returning_rows.push(returning);
        }
        after_row_events.extend(row_after_events);
        if prepared_effect {
            affected += 1;
            has_prepared_effect = true;
        }
        documents.push(document);
        target_tables.push(target_table);
        prepared_conflicts.push(prepared);
    }
    drop(overlay);
    let has_prepared_auto_identity = auto_id_col.is_some() && !stmt.rows.is_empty();
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
    for ((target_table, document), prepared) in target_tables
        .into_iter()
        .zip(documents)
        .zip(&mut prepared_conflicts)
    {
        cancel.check()?;
        let supplied = matches!(
            prepared,
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
        apply_validated_prepared_insert(engine, &target_table, document, prepared, known_new)?;
    }
    crate::sql::triggers::fire_after_row_trigger_events(engine, &after_row_events)?;
    referential_actions.fire_after_statement_triggers(engine)?;
    if let Some(columns) = conflict_update_columns.as_deref() {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Update,
            columns,
        )?;
    }
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &stmt.table,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Insert,
        &[],
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
    Ok(SQLResult::from_affected(affected))
}

fn attach_prepared_insert_identity(
    prepared: PreparedInsertConflict,
    (doc_id, supplied): (DocId, bool),
) -> PreparedInsertConflict {
    match prepared {
        PreparedInsertConflict::Unresolved => PreparedInsertConflict::Insert { doc_id, supplied },
        resolved => resolved,
    }
}

fn stage_prepared_insert_row(
    context: PreparedInsertRowContext<'_>,
    prepared: &mut PreparedInsertConflict,
) -> Result<
    (
        Option<uqa_execution::OwnedPhysicalRow>,
        Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    ),
    SQLError,
> {
    let PreparedInsertRowContext {
        engine,
        stmt,
        storage_table,
        document,
        shared_document,
        conflict_update_columns,
        params,
        scope,
    } = context;
    validate_document_non_key_constraints(engine, storage_table, document, params)?;
    let (images, after_row_events) = match prepared {
        PreparedInsertConflict::Insert { doc_id, .. } => {
            validate_key_constraints(engine, storage_table, document, None)?;
            if let Some(shared_document) = shared_document {
                engine.stage_shared_command_document(
                    storage_table,
                    *doc_id,
                    Some(Arc::clone(shared_document)),
                )?;
            } else {
                engine.stage_command_document(storage_table, *doc_id, Some(document.clone()))?;
            }
            let mut after_row_events = Vec::new();
            if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
                engine,
                crate::sql::triggers::AfterRowTriggerInput {
                    table: storage_table,
                    event: uqa_sql::ast::TriggerEvent::Insert,
                    old_doc_id: *doc_id,
                    new_doc_id: *doc_id,
                    old_document: None,
                    new_document: Some(document),
                    updated_columns: &[],
                },
            )? {
                after_row_events.push(event);
            }
            (
                ReturningRowImages {
                    old: None,
                    new: Some(ReturningRowImage {
                        doc_id: *doc_id,
                        document,
                    }),
                },
                after_row_events,
            )
        }
        PreparedInsertConflict::Updated(prepared) => {
            let old_doc_id = prepared.doc_id;
            let mut after_row_events = Vec::new();
            let doc_id = stage_prepared_document_rewrite(
                engine,
                prepared,
                params,
                Some(conflict_update_columns),
                &mut after_row_events,
            )?;
            (
                ReturningRowImages {
                    old: Some(ReturningRowImage {
                        doc_id: old_doc_id,
                        document: &prepared.old_document,
                    }),
                    new: Some(ReturningRowImage {
                        doc_id,
                        document: &prepared.new_document,
                    }),
                },
                after_row_events,
            )
        }
        PreparedInsertConflict::Skip => return Ok((None, Vec::new())),
        PreparedInsertConflict::Unresolved => {
            return Err(SQLError::Internal(
                "INSERT command overlay has no prepared document identity".into(),
            ))
        }
    };
    let returning = if stmt.returning.is_empty() {
        None
    } else {
        Some(build_returning_row(
            engine,
            ReturningProjectionRow {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                images,
                aliases: &stmt.returning_aliases,
                context: None,
            },
            &stmt.returning,
            params,
            scope,
        )?)
    };
    Ok((returning, after_row_events))
}

fn apply_validated_prepared_insert(
    engine: &Engine,
    table: &str,
    document: Document,
    prepared: &mut PreparedInsertConflict,
    known_new: bool,
) -> Result<bool, SQLError> {
    match prepared {
        PreparedInsertConflict::Skip => Ok(false),
        PreparedInsertConflict::Updated(prepared) => {
            apply_validated_prepared_document_rewrite(engine, prepared)?;
            Ok(true)
        }
        PreparedInsertConflict::Insert { doc_id, .. } => {
            let vectors = document_vectors(engine, table, &document)?;
            engine.add_prepared_document_with_vector_values(
                table, *doc_id, document, vectors, known_new,
            )?;
            Ok(true)
        }
        PreparedInsertConflict::Unresolved => Err(SQLError::Internal(
            "INSERT reached execution without a prepared document identity".into(),
        )),
    }
}

pub(in crate::sql) fn refresh_insert_identity_after_trigger(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &Document,
    identity: &mut (DocId, bool),
) -> Result<(), SQLError> {
    let Some(doc_id) =
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    else {
        return Ok(());
    };
    if doc_id == identity.0 {
        return Ok(());
    }
    let owner = engine.partition_identity_owner(table)?;
    engine
        .advance_next_id(&owner, doc_id)
        .map_err(|error| dml_storage_error("apply BEFORE INSERT trigger identity", error))?;
    *identity = (doc_id, true);
    Ok(())
}

pub(in crate::sql) fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("INSERT defaults", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    for definition in columns {
        let col = definition.name;
        if document.contains_key(&col) {
            continue;
        }
        if definition
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_identity)
        {
            continue;
        }
        if let Some(default_expr) = engine
            .try_column_default_expr(table, &col)
            .map_err(|err| dml_storage_error("INSERT defaults", err))?
        {
            let value = coerce_to_column_type(
                engine,
                table,
                &col,
                eval_lowered_expression(engine, &default_expr, None, params)?,
            )?;
            document.insert(col, value);
        }
    }
    Ok(())
}

pub(in crate::sql) fn document_vectors(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    let mut vectors = BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = engine
            .column_type(table, field)
            .map_err(|err| dml_storage_error("vector extraction", err))?
        else {
            continue;
        };
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(field.clone(), index_vectors_for_type(value, &ty)?);
        }
    }
    Ok(vectors)
}
