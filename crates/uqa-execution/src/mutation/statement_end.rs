//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The statement that a command belongs to, and the AFTER events and written rows that its commands leave for its end.

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::mutation::publication::MutationPublicationBatch;
use crate::mutation::statement::context::MutationStatementContext;
use crate::mutation::triggers::context::TriggerContext;
use crate::mutation::triggers::events::AfterRowEvents;
use crate::mutation::triggers::queue::StatementEvent;
use crate::query::scope::StatementCommands;
use crate::query::CteScope;
use std::cell::RefCell;
use std::sync::Arc;
use uqa_sql::{plan::CtePlan, SQLError, SQLParam};

thread_local! {
    /// The statements this thread runs, outermost first: a statement that the triggers or functions of another start runs above it.
    static RUNNING_STATEMENTS: RefCell<Vec<Arc<StatementCommands>>> = const { RefCell::new(Vec::new()) };
}

/// A statement that runs above the statements that started it until the guard drops.
pub struct RunningStatement(());

impl Drop for RunningStatement {
    fn drop(&mut self) {
        RUNNING_STATEMENTS.with(|statements| {
            statements.borrow_mut().pop();
        });
    }
}

/// Run `statement` above the statements that started it while the returned guard lives.
pub fn enter_statement(statement: Arc<StatementCommands>) -> RunningStatement {
    RUNNING_STATEMENTS.with(|statements| statements.borrow_mut().push(statement));
    RunningStatement(())
}

/// The statement a command belongs to: the one whose WITH holds the command, or a statement of its own, which runs above the statements that started it while the guard lives.
pub fn statement_commands<S: Clone>(
    inherited: Option<&CteScope<S>>,
) -> (Arc<StatementCommands>, Option<RunningStatement>) {
    if let Some(statement) = inherited.and_then(CteScope::statement_commands) {
        return (Arc::clone(statement), None);
    }
    let statement = Arc::<StatementCommands>::default();
    let running = enter_statement(Arc::clone(&statement));
    (statement, Some(running))
}

/// Whether a statement started the running one, whose writes it then finds modified by a later command.
fn started_by_another_statement() -> bool {
    RUNNING_STATEMENTS.with(|statements| statements.borrow().len() > 1)
}

/// Note rows that the running statement wrote for every statement that started it.
fn note_triggered_rows(rows: &[PhysicalDocumentIdentity]) {
    RUNNING_STATEMENTS.with(|statements| {
        let statements = statements.borrow();
        if let Some((_, starters)) = statements.split_last() {
            for starter in starters {
                starter.note_triggered(rows.iter().cloned());
            }
        }
    });
}

/// Fire the BEFORE STATEMENT triggers of `statements` for a command of `statement`, each unless the statement already fired them for its relation and operation.
pub fn fire_before_statements(
    statement: &StatementCommands,
    context: &TriggerContext<'_>,
    statements: &[StatementEvent],
) -> Result<(), SQLError> {
    statements.iter().try_for_each(|event| {
        statement
            .after_triggers()
            .fire_before_statement(context, event)
    })
}

/// End one command of `statement`: queue the AFTER events of its rows and of the relations and operations it wrote, and fire the queue unless the statement's WITH modifies data, whose commands fire theirs together once the statement ends (`AfterTriggerEndQuery`).
pub fn end_command(
    statement: &StatementCommands,
    context: &TriggerContext<'_>,
    statements: &[StatementEvent],
    rows: AfterRowEvents,
) -> Result<(), SQLError> {
    statement
        .after_triggers()
        .queue_command(context, statements, rows)?;
    if statement.modifies_with() {
        return Ok(());
    }
    statement.after_triggers().fire(context)
}

/// A publication batch for a command of `statement`, which keeps the rows it writes when the statement's WITH modifies data or another statement started it.
pub fn publication_batch(statement: &StatementCommands) -> MutationPublicationBatch {
    MutationPublicationBatch::recording_writes(
        statement.modifies_with() || started_by_another_statement(),
    )
}

/// Note the rows a command's publication wrote for the other commands of its statement and for the statements that started it.
pub fn note_written_rows(
    statement: &StatementCommands,
    publication: &mut MutationPublicationBatch,
) {
    let rows = publication.take_written();
    if rows.is_empty() {
        return;
    }
    note_triggered_rows(&rows);
    if statement.modifies_with() {
        statement.note_written(rows);
    }
}

/// Note a row that the running statement wrote without a publication batch, for the statements that started it.
pub fn note_written_row(table: &str, doc_id: uqa_core::DocId) {
    if started_by_another_statement() {
        note_triggered_rows(&[PhysicalDocumentIdentity {
            table: table.to_string(),
            doc_id,
        }]);
    }
}

/// A publication batch for a referential action of the running statement, which keeps the rows it writes when another statement started the running one.
pub fn action_publication_batch() -> MutationPublicationBatch {
    MutationPublicationBatch::recording_writes(started_by_another_statement())
}

/// Note the rows a referential action wrote for the statements that started the running statement.
pub fn note_action_rows(publication: &mut MutationPublicationBatch) {
    note_triggered_rows(&publication.take_written());
}

/// Finish successful command work and release the scope's CTE producers on every result. Data-modifying WITH items and their AFTER events run only after primary success; producer cleanup preserves any primary failure.
pub fn finish_statement<S: Clone + Send + Sync + 'static, T>(
    context: &MutationStatementContext<'_, S>,
    params: &[SQLParam],
    ctes: &[CtePlan],
    scope: Option<&mut CteScope<S>>,
    result: Result<T, SQLError>,
) -> Result<T, SQLError> {
    let Some(scope) = scope else {
        return result;
    };
    let result = result.and_then(|output| {
        if ctes.iter().any(|cte| cte.body.modifies_data()) {
            crate::query::cte::finish_statement_ctes(context.query.source.ctes, params, scope)?;
            if let Some(commands) = scope.statement_commands() {
                commands
                    .after_triggers()
                    .fire(&context.mutation.preparation.referential.triggers)?;
            }
        }
        Ok(output)
    });
    scope.finish_cte_producers(result)
}
