//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The statement that a command belongs to, and the AFTER events and written rows that its commands leave for its end.

use crate::mutation::publication::MutationPublicationBatch;
use crate::mutation::statement::context::MutationStatementContext;
use crate::mutation::triggers::context::TriggerContext;
use crate::mutation::triggers::queue::StatementEvent;
use crate::mutation::triggers::AfterRowTriggerEvent;
use crate::query::scope::StatementCommands;
use crate::query::CteScope;
use std::sync::Arc;
use uqa_sql::{plan::CtePlan, SQLError, SQLParam};

/// The statement a command belongs to: the one whose WITH holds the command, or a statement of its own.
pub fn statement_commands<S: Clone>(inherited: Option<&CteScope<S>>) -> Arc<StatementCommands> {
    inherited
        .and_then(CteScope::statement_commands)
        .cloned()
        .unwrap_or_default()
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
    rows: Vec<AfterRowTriggerEvent>,
) -> Result<(), SQLError> {
    statement
        .after_triggers()
        .queue_command(context, statements, rows)?;
    if statement.modifies_with() {
        return Ok(());
    }
    statement.after_triggers().fire(context)
}

/// A publication batch for a command of `statement`, which keeps the rows it writes when the statement's WITH modifies data.
pub fn publication_batch(statement: &StatementCommands) -> MutationPublicationBatch {
    MutationPublicationBatch::recording_writes(statement.modifies_with())
}

/// Note the rows a command's publication wrote for the other commands of its statement.
pub fn note_written_rows(
    statement: &StatementCommands,
    publication: &mut MutationPublicationBatch,
) {
    if statement.modifies_with() {
        statement.note_written(publication.take_written());
    }
}

/// End a statement whose WITH, `ctes`, modifies data: run the items `scope` kept for after the primary query, then fire every AFTER event the statement's commands queued. A statement whose WITH only reads fired its events when its command ended, and one that did not get as far as its WITH has nothing left to end.
pub fn finish_statement<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    params: &[SQLParam],
    ctes: &[CtePlan],
    scope: Option<&mut CteScope<S>>,
) -> Result<(), SQLError> {
    let Some(scope) = scope.filter(|_| ctes.iter().any(|cte| cte.body.modifies_data())) else {
        return Ok(());
    };
    crate::query::cte::finish_statement_ctes(context.query.source.ctes, params, scope)?;
    let Some(commands) = scope.statement_commands() else {
        return Ok(());
    };
    commands
        .after_triggers()
        .fire(&context.mutation.preparation.referential.triggers)
}
