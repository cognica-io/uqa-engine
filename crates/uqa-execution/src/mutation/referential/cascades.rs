//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The referential actions that `PostgreSQL` takes in the internal AFTER ROW triggers of a referenced row once the statement has written its rows: `RI_FKey_cascade_del`, `RI_FKey_cascade_upd`, `RI_FKey_setnull_*` and `RI_FKey_setdefault_*`. An action runs as a statement of its own on the foreign key's table within the queue of the statement that fired it: its BEFORE STATEMENT triggers fire once for the queue's open state of the table, the BEFORE ROW triggers of its rows fire as it writes them, and the AFTER events of its rows go to the end of the queue.

use super::{
    apply_set_action_to_child, foreign_key_comparison_types, lock_referencing_child,
    prepare_document_delete_excluding, prepare_referential_document_rewrite, referencing_rows,
    ForeignKey, ForeignKeyAction, ForeignKeyComparison, PhysicalDocumentIdentity,
    PreparedDocumentRewrite, ReferencingChildLock, ReferentialContext,
    ReferentialRewritePreparation, SQLError, Value,
};
use crate::mutation::{
    command_scope::MutationOverlayScope,
    prepared::{
        decode_prepared_mutation_action_row, prepared_mutation_action_schema,
        push_prepared_mutation_action, PreparedMutationAction,
    },
    publication::{
        finish_mutation_publication, publish_prepared_mutation_action, InsertedIdentity,
    },
    staging::{stage_prepared_document_delete, stage_referential_rewrite},
    statement::MutationExecutionContext,
    triggers::queue::{AfterTriggerQueue, StatementEvent},
};
use std::sync::Arc;
use uqa_sql::ast::TriggerEvent;

/// A referential action that a delete or an update of a referenced row queued.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferentialAction {
    /// The foreign key's table, whose rows the action writes.
    pub(super) constraint_table: String,
    pub(super) foreign_key: Arc<ForeignKey>,
    /// The key the referenced row held.
    #[serde(with = "crate::mutation::triggers::events::values::list")]
    pub(super) key: Vec<Value>,
    /// The key an update gave the referenced row, which `ON UPDATE CASCADE` writes into the referencing rows; `None` for a delete.
    #[serde(with = "crate::mutation::triggers::events::values::optional_list")]
    pub(super) new_key: Option<Vec<Value>>,
}

impl ReferentialAction {
    fn action(&self) -> ForeignKeyAction {
        if self.new_key.is_some() {
            self.foreign_key.on_update
        } else {
            self.foreign_key.on_delete
        }
    }

    /// The operation of the action's statement and the columns it sets.
    fn statement(&self) -> Result<StatementEvent, SQLError> {
        let foreign_key = self.foreign_key.as_ref();
        let (event, columns) = match (self.action(), self.new_key.is_some()) {
            (ForeignKeyAction::Cascade, false) => (TriggerEvent::Delete, Vec::new()),
            (ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault, false) => {
                (TriggerEvent::Update, delete_set_columns(foreign_key))
            }
            (
                ForeignKeyAction::Cascade
                | ForeignKeyAction::SetNull
                | ForeignKeyAction::SetDefault,
                true,
            ) => (TriggerEvent::Update, foreign_key.local_columns.clone()),
            (ForeignKeyAction::NoAction | ForeignKeyAction::Restrict, _) => {
                return Err(SQLError::Internal(format!(
                    "foreign key `{}` queued a referential action under {:?}",
                    foreign_key.name.as_deref().unwrap_or("<unnamed>"),
                    self.action()
                )))
            }
        };
        Ok(StatementEvent::new(&self.constraint_table, event, &columns))
    }
}

/// The columns that `ON DELETE SET NULL` or `ON DELETE SET DEFAULT` sets: the listed ones, or every column of the foreign key.
fn delete_set_columns(foreign_key: &ForeignKey) -> Vec<String> {
    if foreign_key.on_delete_set_columns.is_empty() {
        foreign_key.local_columns.clone()
    } else {
        foreign_key.on_delete_set_columns.clone()
    }
}

/// Take a queued referential action: delete or rewrite every row of the foreign key's table that references the key, as one statement in `queue`.
pub(super) fn run_referential_action<S: Clone + 'static>(
    context: &MutationExecutionContext<'_, S>,
    action: &ReferentialAction,
    queue: &AfterTriggerQueue,
) -> Result<(), SQLError> {
    let referential = &context.preparation.referential;
    let foreign_key = action.foreign_key.as_ref();
    let table = action.constraint_table.as_str();
    let kind = action.action();
    let statement = action.statement()?;
    referential
        .locking
        .session
        .lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    queue.fire_before_statement(&referential.triggers, &statement)?;
    let comparison = foreign_key_comparison_types(
        referential.constraints.partitions.catalog,
        table,
        foreign_key,
    )?;
    let expected = comparison.normalize_referenced(action.key.clone())?;
    let referencing = referencing_rows(
        referential,
        table,
        foreign_key,
        &comparison,
        &expected,
        kind,
    )?;
    let deleting = statement.event == TriggerEvent::Delete;
    let work_mem = referential.constraints.memory.work_mem_bytes()?;
    let cancellation = referential.triggers.runtime.cancellation;
    let deleted = collect_deleted_children(&referencing, deleting, work_mem, cancellation)?;
    let action_schema = prepared_mutation_action_schema();
    let mut prepared = crate::SpillBuffer::new(work_mem);
    let mut events = crate::mutation::triggers::events::AfterRowEvents::default();
    let overlay = MutationOverlayScope::new(context.state);
    for child in referencing.read_rows()? {
        let child = child?;
        if deleting {
            if let Some(delete) = prepare_document_delete_excluding(
                referential,
                &child.table,
                child.doc_id,
                &|table, id| deleted.contains(table, id),
                true,
            )? {
                stage_prepared_document_delete(context.preparation.staging, &delete, &mut events)?;
                push_prepared_mutation_action(
                    &mut prepared,
                    &action_schema,
                    PreparedMutationAction::Delete(delete),
                )?;
            }
            continue;
        }
        let Some(mut rewrite) = prepare_child_rewrite(
            referential,
            action,
            &statement,
            &child,
            &comparison,
            &expected,
        )?
        else {
            continue;
        };
        stage_referential_rewrite(context.preparation.staging, &mut rewrite, &[], &mut events)?;
        push_prepared_mutation_action(
            &mut prepared,
            &action_schema,
            PreparedMutationAction::Rewrite(rewrite),
        )?;
    }
    let published = overlay.finish();
    if prepared.rows() != 0 {
        context.state.prepare_writer()?;
        let mut publication =
            crate::mutation::statement_end::action_publication_batch().with_published(published);
        for row in prepared
            .drain_rows()
            .map_err(crate::physical::physical_exec_error)?
        {
            cancellation.check()?;
            let action = decode_prepared_mutation_action_row(
                row.map_err(crate::physical::physical_exec_error)?,
            )?;
            publish_prepared_mutation_action(
                context.publication,
                action,
                InsertedIdentity::Unknown,
                &mut publication,
            )?;
        }
        finish_mutation_publication(context.publication, &mut publication)?;
        crate::mutation::statement_end::note_action_rows(&mut publication);
    }
    queue.queue_command(&referential.triggers, &[statement], events)
}

fn collect_deleted_children(
    referencing: &super::references::ReferencingRows,
    deleting: bool,
    work_mem: usize,
    cancellation: &uqa_core::CancellationToken,
) -> Result<crate::mutation::row_membership::PhysicalRows, SQLError> {
    let mut deleted = crate::mutation::row_membership::PhysicalRowBuffer::new(
        work_mem,
        cancellation.clone(),
        "referential deleted rows",
    );
    if deleting {
        for child in referencing.read_rows()? {
            let child = child?;
            deleted.insert(&child.table, child.doc_id)?;
        }
    }
    deleted.finish()
}

/// Lock one row that references the key and prepare its rewrite: `ON UPDATE CASCADE` writes the referenced row's new key into it, and `SET NULL` or `SET DEFAULT` sets the action's columns. `None` when the row no longer references the key or a BEFORE ROW trigger skipped it.
fn prepare_child_rewrite<S: Clone + 'static>(
    referential: &ReferentialContext<'_, S>,
    action: &ReferentialAction,
    statement: &StatementEvent,
    child: &PhysicalDocumentIdentity,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    let foreign_key = action.foreign_key.as_ref();
    let table = action.constraint_table.as_str();
    let Some((child, document)) = lock_referencing_child(
        referential,
        ReferencingChildLock {
            ref_table: table,
            child,
            lock_columns: &statement.columns,
            foreign_key,
            comparison,
            expected,
        },
    )?
    else {
        return Ok(None);
    };
    let mut updated = document.clone();
    match (&action.new_key, action.action()) {
        (Some(new_key), ForeignKeyAction::Cascade) => {
            for ((column, value), source) in foreign_key
                .local_columns
                .iter()
                .zip(new_key)
                .zip(&comparison.referenced_types)
            {
                updated.insert(
                    column.clone(),
                    uqa_sql::assignment::columns::coerce_to_column_type_from(
                        referential.assignment.assignment,
                        referential.assignment.columns,
                        &child.table,
                        column,
                        value.clone(),
                        Some(source),
                    )?,
                );
            }
        }
        (_, kind) => apply_set_action_to_child(
            referential,
            &child.table,
            &document,
            &mut updated,
            &statement.columns,
            kind,
        )?,
    }
    prepare_referential_document_rewrite(
        referential,
        ReferentialRewritePreparation {
            constraint_table: table,
            table: &child.table,
            doc_id: child.doc_id,
            old_document: document,
            proposed_document: updated,
            updated_columns: statement.columns.clone(),
        },
        &[],
    )
}
