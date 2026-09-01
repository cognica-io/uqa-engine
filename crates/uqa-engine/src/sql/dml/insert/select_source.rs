//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    apply_missing_column_defaults, attach_prepared_insert_identity, coerce_to_column_type,
    dml_storage_error, encode_prepared_insert_spill_row, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, partition_insert_target,
    physical_work_mem_bytes, prepare_auto_increment_identity, prepare_insert_identity,
    prepared_insert_spill_schema, refresh_insert_identity_after_trigger, stage_prepared_insert_row,
    validate_mutation_columns, Arc, CteScope, Document, Engine, InsertConflictLocks,
    InsertConflictPreparation, InsertPlan, PreparedInsertConflict, PreparedInsertSpillRow, RefCell,
    SQLError, SQLParam,
};

pub(super) struct InsertSelectConsumer {
    pub(super) state: RefCell<InsertSelectConsumerState>,
}

pub(super) struct InsertSelectIdentity {
    pub(super) auto_id_column: Option<String>,
    pub(super) id_column: String,
    pub(super) accepts_supplied_identity: bool,
}

pub(super) struct InsertSelectConsumerState {
    pub(super) stmt: InsertPlan,
    pub(super) params: Vec<SQLParam>,
    pub(super) snapshot_scope: CteScope,
    pub(super) auto_id_column: Option<String>,
    pub(super) id_column: String,
    pub(super) accepts_supplied_identity: bool,
    pub(super) conflict_update_columns: Vec<String>,
    pub(super) columns: Option<Vec<String>>,
    pub(super) result_width: Option<usize>,
    pub(super) prepared_schema: uqa_execution::RowSchema,
    pub(super) prepared_buffer: Option<uqa_execution::SpillBuffer>,
    pub(super) conflict_locks: Option<InsertConflictLocks>,
    pub(super) affected: u64,
    pub(super) returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    pub(super) events: super::super::MutationEventQueue,
    pub(super) has_prepared_effect: bool,
    pub(super) has_prepared_auto_identity: bool,
}

pub(super) struct PreparedInsertSelect {
    pub(super) rows: uqa_execution::SharedSpill,
    pub(super) conflict_locks: InsertConflictLocks,
    pub(super) affected: u64,
    pub(super) returning_rows: Vec<uqa_execution::OwnedPhysicalRow>,
    pub(super) events: super::super::MutationEventQueue,
    pub(super) has_prepared_effect: bool,
    pub(super) has_prepared_auto_identity: bool,
}

pub(super) struct PreparedInsertRowContext<'a> {
    pub(super) engine: &'a Engine,
    pub(super) stmt: &'a InsertPlan,
    pub(super) storage_table: &'a str,
    pub(super) document: &'a Document,
    pub(super) shared_document: Option<&'a Arc<Document>>,
    pub(super) conflict_update_columns: &'a [String],
    pub(super) params: &'a [SQLParam],
    pub(super) scope: &'a CteScope,
}

impl InsertSelectConsumer {
    pub(super) fn new(
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
                events: super::super::MutationEventQueue::default(),
                has_prepared_effect: false,
                has_prepared_auto_identity: false,
            }),
        })
    }

    pub(super) fn take_prepared(&self) -> Result<PreparedInsertSelect, SQLError> {
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

    #[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
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
                .unwrap_or(super::super::Value::Null);
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
        super::super::stamp_tuple_xmin(engine, &target_table, &mut document)?;
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
