//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming INSERT SELECT row preparation, with scopes bound by the statement owner.
use super::{
    codec::{prepared_insert_spill_schema, push_prepared_insert_spill_row, PreparedInsertSpillRow},
    rows::{attach_prepared_insert_identity, stage_prepared_insert_row, PreparedInsertRowContext},
};
use crate::mutation::{
    assignment::apply_missing_column_defaults,
    conflict::update::{InsertConflictLocks, InsertConflictPreparation},
    constraints::{lock_document_key_dependencies, partition_insert_target, ConstraintStatement},
    identity::{
        prepare_auto_increment_identity, prepare_insert_identity,
        refresh_insert_identity_after_trigger, InsertIdentityContext,
    },
    prepared::PreparedInsertConflict,
};
use crate::query::{projection::physical_work_mem_bytes, runtime::QueryRuntimeView, CteScope};
use std::cell::RefCell;
use uqa_core::memory::{Produced, ProductionVec};
use uqa_sql::{plan::InsertPlan, SQLError, SQLParam};
use uqa_storage::document_store::Document;
mod columns;
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
    production: uqa_core::memory::OwnedProductionControl,
}

pub struct InsertSelectIdentity {
    pub auto_id_column: Option<String>,
    pub id_column: String,
    pub identity_source: crate::mutation::identity::IdentitySource,
}

pub struct InsertSelectConsumerState<S: Clone + 'static> {
    pub stmt: Produced<InsertPlan>,
    pub params: Produced<Vec<SQLParam>>,
    pub snapshot_scope: CteScope<S>,
    pub auto_id_column: Option<Produced<String>>,
    pub id_column: Produced<String>,
    pub identity_source: crate::mutation::identity::IdentitySource,
    pub conflict_update_columns: Produced<Vec<String>>,
    columns: Option<columns::InsertSelectColumns>,
    /// The columns the statement supplies, which its constraint violations show to a role that may not read the table: the target columns the source fills and those `ON CONFLICT DO UPDATE` sets.
    pub supplied_columns: Produced<Vec<String>>,
    /// The catalog name of the relation the statement names.
    pub statement_relation: Produced<String>,
    /// The identity columns whose source values `OVERRIDING USER VALUE` discards.
    pub discarded_identities: uqa_core::memory::OwnedSet<uqa_core::memory::Produced<String>>,
    pub result_width: Option<usize>,
    pub prepared_schema: crate::RowSchema,
    pub prepared_buffer: Option<crate::SpillBuffer>,
    pub conflict_locks: Option<InsertConflictLocks>,
    pub affected: u64,
    pub returning_rows: crate::mutation::returning::ReturningRows,
    pub events: crate::mutation::events::MutationEventQueue,
    pub has_prepared_effect: bool,
    pub has_prepared_auto_identity: bool,
    pub supplied_identities: super::supplied_identities::SuppliedIdentities,
}

pub struct PreparedInsertSelect {
    pub rows: crate::SharedSpill,
    pub conflict_locks: InsertConflictLocks,
    pub affected: u64,
    pub returning_rows: crate::mutation::returning::ReturningRows,
    pub events: crate::mutation::events::MutationEventQueue,
    pub has_prepared_effect: bool,
    pub has_prepared_auto_identity: bool,
    pub supplied_identities: super::supplied_identities::SuppliedIdentities,
}

impl<S: Clone + 'static> InsertSelectConsumer<S> {
    pub fn new(
        services: InsertSourceContext<'_, S>,
        stmt: &InsertPlan,
        params: &[SQLParam],
        snapshot_scope: CteScope<S>,
        identity: &InsertSelectIdentity,
        conflict_update_columns: &[String],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let stmt = stmt.clone_with_control(control)?;
        let mut captured_params = ProductionVec::new(*control);
        captured_params.reserve(params.len())?;
        for param in params {
            captured_params.push_produced(param.clone_with_control(control)?)?;
        }
        let params = captured_params.finish()?;
        let auto_id_column = identity
            .auto_id_column
            .as_ref()
            .map(|name| control.copy_text(name))
            .transpose()?;
        let id_column = control.copy_text(&identity.id_column)?;
        let mut conflicts = ProductionVec::new(*control);
        conflicts.reserve(conflict_update_columns.len())?;
        for name in conflict_update_columns {
            conflicts.push_produced(control.copy_text(name)?)?;
        }
        let conflict_update_columns = conflicts.finish()?;
        let prepared_schema = prepared_insert_spill_schema();
        Ok(Self {
            production: control.capture(),
            state: RefCell::new(InsertSelectConsumerState {
                stmt,
                params,
                snapshot_scope,
                auto_id_column,
                id_column,
                identity_source: identity.identity_source,
                conflict_update_columns,
                columns: None,
                supplied_columns: control.finish(Vec::new(), control.empty_reservation())?,
                statement_relation: control.copy_text("")?,
                discarded_identities: uqa_core::memory::OwnedSet::new(),
                result_width: None,
                prepared_schema,
                prepared_buffer: Some(crate::SpillBuffer::new(
                    physical_work_mem_bytes(services.runtime)?.max(1),
                )),
                conflict_locks: Some(InsertConflictLocks::new(&services.rows.referential)),
                affected: 0,
                returning_rows: crate::mutation::returning::ReturningRows::new(services.runtime)?,
                events: crate::mutation::events::MutationEventQueue::default(),
                has_prepared_effect: false,
                has_prepared_auto_identity: false,
                supplied_identities: super::supplied_identities::SuppliedIdentities::default(),
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
            supplied_identities: std::mem::take(&mut state.supplied_identities),
        })
    }
}

impl<S: Clone + 'static> InsertSelectConsumer<S> {
    #[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
    pub fn consume(
        &self,
        services: InsertSourceContext<'_, S>,
        source_row: crate::OwnedPhysicalRow,
        destination: Option<crate::mutation::returning::ReturningDestination<'_>>,
    ) -> Result<crate::query::consumer::QueryConsumerControl, SQLError> {
        services.runtime.check_cancelled()?;
        let mut state = self.state.borrow_mut();
        let InsertSelectConsumerState {
            stmt,
            params,
            snapshot_scope,
            auto_id_column,
            id_column,
            identity_source,
            conflict_update_columns,
            columns,
            supplied_columns,
            statement_relation,
            discarded_identities,
            result_width,
            prepared_schema,
            prepared_buffer,
            conflict_locks,
            affected,
            returning_rows,
            events,
            has_prepared_effect,
            has_prepared_auto_identity,
            supplied_identities,
        } = &mut *state;
        let columns = columns
            .as_ref()
            .ok_or_else(|| {
                SQLError::Internal("INSERT SELECT row consumer was not initialized".into())
            })?
            .as_slice(stmt);
        let result_width = result_width.ok_or_else(|| {
            SQLError::Internal("INSERT SELECT row consumer has no source width".into())
        })?;
        let source_schema = &source_row.schema;
        let source_row = source_row.view();
        let mut document = Document::new();
        for (index, column) in columns.iter().take(result_width).enumerate() {
            if discarded_identities.contains(&column.column) {
                continue;
            }
            if uqa_sql::assignment::columns::generated_column_kind(
                services.rows.referential.assignment.columns,
                &stmt.table,
                &column.column,
            )?
            .is_some()
            {
                if uqa_sql::semantics::generated_values::insert_has_written_value(stmt, index) {
                    return Err(
                        uqa_sql::semantics::generated_values::generated_column_insert_error(
                            &column.column,
                        ),
                    );
                }
                continue;
            }
            let value = source_row
                .value_at(index)
                .cloned()
                .unwrap_or(uqa_core::Value::Null);
            let value = crate::mutation::assignment::coerce_mutation_assignment(
                services.rows.referential.assignment,
                snapshot_scope,
                crate::mutation::assignment::MutationAssignmentTarget {
                    table: &stmt.table,
                    target: column,
                    current: document.get(&column.column),
                    final_column_write: !columns[index + 1..]
                        .iter()
                        .any(|next| next.column == column.column),
                    action: "INSERT SELECT",
                    new_row: true,
                },
                value,
                source_schema.column_types()[index].as_ref(),
                None,
                params,
            )?;
            document.insert(column.column.clone(), value);
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
            *identity_source,
            auto_id_column.as_ref().map(|name| name.as_str()),
            &mut document,
            "prepare INSERT SELECT identity",
        )?;
        *has_prepared_auto_identity |= prepared_auto_identity.is_some();
        let statement = ConstraintStatement::new(statement_relation, supplied_columns);
        let target_table = partition_insert_target(
            services.rows.referential.constraints,
            statement,
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
                *identity_source,
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
            *identity_source,
            &document,
            &mut insert_identity,
        )?;
        let trigger_target = partition_insert_target(
            services.rows.referential.constraints,
            statement,
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
        let prepared_conflict = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            conflict_locks
                .as_mut()
                .ok_or_else(|| {
                    SQLError::Internal("INSERT SELECT conflict locks are unavailable".into())
                })?
                .prepare_document(InsertConflictPreparation {
                    context: services.rows.referential,
                    target_table: &stmt.table,
                    table: &target_table,
                    target_qualifier: &stmt.target_qualifier,
                    on_conflict,
                    document: &document,
                    params,
                    scope: snapshot_scope,
                })?
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
                statement,
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
            returning_rows.push_to(row, destination)?;
        }
        events.append_after_rows(row_after_events, &services.rows.referential.triggers)?;
        supplied_identities.note(&target_table, &prepared_conflict);
        push_prepared_insert_spill_row(
            prepared_buffer.as_mut().ok_or_else(|| {
                SQLError::Internal("INSERT SELECT prepared buffer is unavailable".into())
            })?,
            prepared_schema,
            PreparedInsertSpillRow {
                target_table,
                document,
                conflict: prepared_conflict,
            },
        )?;
        if prepared_effect {
            *affected += 1;
            *has_prepared_effect = true;
        }
        Ok(crate::query::consumer::QueryConsumerControl::Continue)
    }
}

use crate::mutation::preparation::MutationPreparationContext;

pub mod binding;
