//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The AFTER events and written rows of the commands of a statement whose WITH modifies data, which wait for the statement to end.

use crate::mutation::publication::MutationPublicationBatch;
use crate::mutation::statement::context::MutationStatementContext;
use crate::mutation::triggers::context::TriggerContext;
use crate::query::CteScope;
use uqa_sql::{plan::CtePlan, SQLError, SQLParam};

/// Fire a command's AFTER events now, or, when its statement's WITH modifies data, queue them until the statement ends. `PostgreSQL` queues every AFTER event of a query and fires the queue only once the primary query and every data-modifying WITH item have finished (`AfterTriggerEndQuery`).
pub fn fire_after_events<S: Clone>(
    scope: &CteScope<S>,
    context: &TriggerContext<'_>,
    fire: impl FnOnce(&TriggerContext<'_>) -> Result<(), SQLError> + Send + 'static,
) -> Result<(), SQLError> {
    match scope.statement_commands() {
        Some(commands) => {
            commands.queue_after_events(Box::new(fire));
            Ok(())
        }
        None => fire(context),
    }
}

/// A publication batch for a command of `scope`'s statement, which keeps the rows it writes when the statement's WITH modifies data.
pub fn publication_batch<S: Clone>(scope: &CteScope<S>) -> MutationPublicationBatch {
    MutationPublicationBatch::recording_writes(scope.statement_commands().is_some())
}

/// Note the rows a command's publication wrote for the other commands of its statement.
pub fn note_written_rows<S: Clone>(
    scope: &CteScope<S>,
    publication: &mut MutationPublicationBatch,
) {
    if let Some(commands) = scope.statement_commands() {
        commands.note_written(publication.take_written());
    }
}

/// End a statement whose WITH, `ctes`, modifies data: run the items `scope` kept for after the primary query, then fire every AFTER event the statement's commands queued, in queue order. A statement whose WITH only reads, or that did not get as far as its WITH, has nothing left to end.
pub fn finish_statement<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    params: &[SQLParam],
    ctes: &[CtePlan],
    scope: Option<&mut CteScope<S>>,
) -> Result<(), SQLError> {
    let Some(scope) = scope.filter(|_| ctes.iter().any(|cte| cte.body.modifies_data())) else {
        return Ok(());
    };
    let events =
        crate::query::cte::finish_statement_ctes(context.query.source.ctes, params, scope)?;
    let triggers = &context.mutation.preparation.referential.triggers;
    events.into_iter().try_for_each(|fire| fire(triggers))
}
