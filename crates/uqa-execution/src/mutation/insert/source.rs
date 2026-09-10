//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming INSERT SELECT row preparation, with scopes bound by the statement owner.
use super::{
    codec::{
        encode_prepared_insert_spill_row, prepared_insert_spill_schema, PreparedInsertSpillRow,
    },
    rows::{attach_prepared_insert_identity, stage_prepared_insert_row, PreparedInsertRowContext},
};
use crate::mutation::{
    assignment::apply_missing_column_defaults,
    conflict::update::{InsertConflictLocks, InsertConflictPreparation},
    constraints::{
        lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
    },
    errors::dml_storage_error,
    identity::{
        prepare_auto_increment_identity, prepare_insert_identity,
        refresh_insert_identity_after_trigger, InsertIdentityContext,
    },
    prepared::PreparedInsertConflict,
};
use crate::query::{projection::physical_work_mem_bytes, runtime::QueryRuntimeView, CteScope};
use std::cell::RefCell;
use uqa_sql::{
    plan::InsertPlan, semantics::partition::partition_insert_target, SQLError, SQLParam,
};
use uqa_storage::document_store::Document;
#[derive(Clone)]
pub struct InsertSourceContext<'a, S: Clone + 'static> {
    pub rows: MutationPreparationContext<'a, S>,
    pub identities: InsertIdentityContext<'a>,
    pub runtime: QueryRuntimeView<'a>,
}
impl<S: Clone + 'static> Copy for InsertSourceContext<'_, S> {}
pub fn insert_source_expression_rows(
    result: uqa_sql::SQLResult,
) -> Result<Vec<Vec<crate::ScalarExpr>>, SQLError> {
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
        .map(|row| row.into_iter().map(crate::ScalarExpr::Literal).collect())
        .collect())
}

pub struct InsertSelectConsumer<S: Clone + 'static> {
    pub state: RefCell<InsertSelectConsumerState<S>>,
}

pub struct InsertSelectIdentity {
    pub auto_id_column: Option<String>,
    pub id_column: String,
    pub accepts_supplied_identity: bool,
}

pub struct InsertSelectConsumerState<S: Clone + 'static> {
    pub stmt: InsertPlan,
    pub params: Vec<SQLParam>,
    pub snapshot_scope: CteScope<S>,
    pub auto_id_column: Option<String>,
    pub id_column: String,
    pub accepts_supplied_identity: bool,
    pub conflict_update_columns: Vec<String>,
    pub columns: Option<Vec<String>>,
    pub result_width: Option<usize>,
    pub prepared_schema: crate::RowSchema,
    pub prepared_buffer: Option<crate::SpillBuffer>,
    pub conflict_locks: Option<InsertConflictLocks>,
    pub affected: u64,
    pub returning_rows: Vec<crate::OwnedPhysicalRow>,
    pub events: crate::mutation::events::MutationEventQueue,
    pub has_prepared_effect: bool,
    pub has_prepared_auto_identity: bool,
}

pub struct PreparedInsertSelect {
    pub rows: crate::SharedSpill,
    pub conflict_locks: InsertConflictLocks,
    pub affected: u64,
    pub returning_rows: Vec<crate::OwnedPhysicalRow>,
    pub events: crate::mutation::events::MutationEventQueue,
    pub has_prepared_effect: bool,
    pub has_prepared_auto_identity: bool,
}

impl<S: Clone + 'static> InsertSelectConsumer<S> {
    pub fn new(
        services: InsertSourceContext<'_, S>,
        stmt: &InsertPlan,
        params: &[SQLParam],
        snapshot_scope: CteScope<S>,
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
                prepared_buffer: Some(crate::SpillBuffer::new(
                    physical_work_mem_bytes(services.runtime)?.max(1),
                )),
                conflict_locks: Some(InsertConflictLocks::new(&services.rows.referential)),
                affected: 0,
                returning_rows: Vec::new(),
                events: crate::mutation::events::MutationEventQueue::default(),
                has_prepared_effect: false,
                has_prepared_auto_identity: false,
            }),
        })
    }

    pub fn take_prepared(&self) -> Result<PreparedInsertSelect, SQLError> {
        let mut state = self.state.borrow_mut();
        let buffer = state.prepared_buffer.take().ok_or_else(|| {
            SQLError::Internal("INSERT SELECT row consumer was finalized more than once".into())
        })?;
        let rows = buffer
            .into_shared(state.prepared_schema.clone())
            .map_err(crate::query::projection::physical_exec_error)?;
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

impl<S: Clone + 'static> InsertSelectConsumer<S> {
    pub fn begin(
        &self,
        services: InsertSourceContext<'_, S>,
        source_columns: &[String],
        _schema: &crate::RowSchema,
    ) -> Result<(), SQLError> {
        let mut state = self.state.borrow_mut();
        let result_width = source_columns.len();
        let implicit_columns = state.stmt.columns.is_empty();
        let columns = if implicit_columns {
            let target_columns = services
                .rows
                .referential
                .assignment
                .rows
                .relations
                .column_names(&state.stmt.table)
                .map_err(|error| dml_storage_error("INSERT SELECT", error))?;
            if target_columns.is_empty() {
                source_columns.to_vec()
            } else {
                target_columns
            }
        } else {
            state.stmt.columns.clone()
        };
        uqa_sql::assignment::columns::validate_mutation_columns(
            services.rows.referential.assignment.columns,
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
    pub fn consume(
        &self,
        services: InsertSourceContext<'_, S>,
        source_row: crate::OwnedPhysicalRow,
    ) -> Result<crate::query::consumer::QueryConsumerControl, SQLError> {
        services.runtime.check_cancelled()?;
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
        let source_schema = &source_row.schema;
        let source_row = source_row.view();
        let mut document = Document::new();
        for (index, column) in columns.iter().take(result_width).enumerate() {
            if uqa_sql::assignment::columns::generated_column_kind(
                services.rows.referential.assignment.columns,
                &stmt.table,
                column,
            )?
            .is_some()
            {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{column}` is a generated column; only DEFAULT may be assigned"
                )));
            }
            let value = source_row
                .value_at(index)
                .cloned()
                .unwrap_or(uqa_core::Value::Null);
            document.insert(
                column.clone(),
                uqa_sql::assignment::columns::coerce_to_column_type_from(
                    services.rows.referential.assignment.assignment,
                    services.rows.referential.assignment.columns,
                    &stmt.table,
                    column,
                    value,
                    source_schema.column_types()[index].as_ref(),
                )?,
            );
        }
        apply_missing_column_defaults(
            services.rows.referential.assignment,
            &stmt.table,
            &mut document,
            params,
        )?;
        let prepared_auto_identity = prepare_auto_increment_identity(
            services.identities,
            &stmt.table,
            id_column,
            auto_id_column.as_deref(),
            &mut document,
            "prepare INSERT SELECT identity",
        )?;
        *has_prepared_auto_identity |= prepared_auto_identity.is_some();
        let target_table = partition_insert_target(
            &services.rows.referential.constraints.partitions,
            &stmt.table,
            &document,
            params,
            stmt.include_descendants,
        )?;
        services.rows.referential.locking.session.lock_relation(
            &target_table,
            crate::row_locks::RelationLockMode::RowExclusive,
        )?;
        let mut insert_identity = match prepared_auto_identity {
            Some(identity) => identity,
            None => prepare_insert_identity(
                services.identities,
                &target_table,
                id_column,
                *accepts_supplied_identity,
                None,
                &mut document,
                "prepare INSERT SELECT identity",
            )?,
        };
        let Some(triggered_document) = crate::mutation::triggers::fire_before_row_triggers(
            &services.rows.referential.triggers,
            &target_table,
            uqa_sql::ast::TriggerEvent::Insert,
            insert_identity.0,
            None,
            Some(&document),
            &[],
        )?
        else {
            return Ok(crate::query::consumer::QueryConsumerControl::Continue);
        };
        document = triggered_document;
        crate::mutation::assignment::refresh_stored_generated_columns(
            services.rows.referential.assignment,
            &target_table,
            &mut document,
        )?;
        refresh_insert_identity_after_trigger(
            crate::mutation::identity::IdentityAllocationContext {
                identifiers: services.rows.referential.identifiers,
                partitions: services.rows.referential.constraints.partitions.catalog,
            },
            &target_table,
            id_column,
            *accepts_supplied_identity,
            auto_id_column.as_deref(),
            &document,
            &mut insert_identity,
        )?;
        let trigger_target = partition_insert_target(
            &services.rows.referential.constraints.partitions,
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
        lock_existing_document_foreign_key_dependencies(
            services.rows.referential.constraints,
            &target_table,
            &document,
        )?;
        let prepared_conflict = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            conflict_locks
                .as_mut()
                .ok_or_else(|| {
                    SQLError::Internal("INSERT SELECT conflict locks are unavailable".into())
                })?
                .prepare_document(
                    InsertConflictPreparation {
                        context: services.rows.referential,
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
            let _key_locks = lock_document_key_dependencies(
                services.rows.referential.constraints,
                &target_table,
                &document,
                None,
            )?;
            PreparedInsertConflict::Unresolved
        };
        let mut prepared_conflict =
            attach_prepared_insert_identity(prepared_conflict, insert_identity);
        let prepared_effect = !matches!(&prepared_conflict, PreparedInsertConflict::Skip);
        let (returning, row_after_events) = stage_prepared_insert_row(
            PreparedInsertRowContext {
                services: services.rows,
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
            .push(crate::Batch::from_physical_rows(
                prepared_schema.clone(),
                vec![encode_prepared_insert_spill_row(PreparedInsertSpillRow {
                    target_table,
                    document,
                    conflict: prepared_conflict,
                })],
            ))
            .map_err(crate::query::projection::physical_exec_error)?;
        if prepared_effect {
            *affected += 1;
            *has_prepared_effect = true;
        }
        Ok(crate::query::consumer::QueryConsumerControl::Continue)
    }
}

use crate::mutation::preparation::MutationPreparationContext;

pub mod binding;
