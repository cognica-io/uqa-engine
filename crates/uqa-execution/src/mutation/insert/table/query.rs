//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming INSERT query delivery, staged row publication and completion.

use super::{
    apply_validated_prepared_insert, decode_prepared_insert_spill_row, dml_returning_result,
    finish_mutation_publication, insert_statement_events, persist_auto_increment_identity,
    statement_end, Arc, CteScope, DmlReturningShape, InsertPlan, InsertSelectConsumer,
    InsertSelectIdentity, KnownNewInserts, MutationOutput, MutationOverlayScope,
    MutationStatementContext, PreparedInsertSelect, PreparedInsertSpillRow, QueryPlan, Rc,
    SQLError, SQLParam,
};
use crate::query::{scope::StatementCommands, statement::consumer::QueryOutputMode};

pub(super) struct InsertQueryExecution<'a, S: Clone + 'static> {
    pub context: &'a MutationStatementContext<'a, S>,
    pub read_context: &'a MutationStatementContext<'a, S>,
    pub stmt: &'a InsertPlan,
    pub source: &'a QueryPlan,
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
    pub identity: InsertSelectIdentity,
    pub conflict_update_columns: Option<&'a [String]>,
    pub statement_commands: &'a Arc<StatementCommands>,
    pub control: uqa_core::memory::ProductionControl<'a>,
}

pub(super) fn run<S: Clone + Send + Sync + 'static>(
    request: InsertQueryExecution<'_, S>,
) -> Result<MutationOutput, SQLError> {
    let InsertQueryExecution {
        context,
        read_context,
        stmt,
        source,
        params,
        scope,
        identity,
        conflict_update_columns,
        statement_commands: _,
        control,
    } = &request;
    let mutation = &context.mutation;
    let preparation = mutation.preparation;
    let snapshot_scope = scope.returning_statement_snapshot_scope();
    let mut source_scope = snapshot_scope.clone();
    source_scope.enable_command_progress_streaming();
    let consumer = Rc::new(InsertSelectConsumer::new(
        context.insert_source(),
        stmt,
        params,
        snapshot_scope,
        identity,
        conflict_update_columns.unwrap_or(&[]),
        control,
    )?);
    let overlay = MutationOverlayScope::new(mutation.state);
    crate::query::statement::execute_query_plan_output(
        &read_context.query,
        source,
        params,
        &mut source_scope,
        QueryOutputMode::RowConsumer(Rc::new(
            crate::mutation::insert::source::binding::InsertSelectOutput {
                binding: mutation.insert_consumers,
                destination: preparation.returning.destination,
                consumer: Rc::clone(&consumer),
            },
        )),
    )?;
    let prepared = consumer.take_prepared()?;
    let published = overlay.finish();
    finish(request, prepared, published)
}

fn finish<S: Clone + Send + Sync + 'static>(
    request: InsertQueryExecution<'_, S>,
    prepared: PreparedInsertSelect,
    published: Option<crate::mutation::overlay::CommandMutationOverlay>,
) -> Result<MutationOutput, SQLError> {
    let InsertQueryExecution {
        context,
        stmt,
        params,
        scope,
        identity,
        conflict_update_columns,
        statement_commands,
        ..
    } = request;
    let InsertSelectIdentity {
        auto_id_column: auto_id_col,
        id_column,
        ..
    } = identity;
    let mutation = &context.mutation;
    let preparation = mutation.preparation;
    let triggers = preparation.referential.triggers;
    let PreparedInsertSelect {
        rows: prepared_rows,
        conflict_locks,
        affected,
        returning_rows,
        events,
        has_prepared_effect,
        has_prepared_auto_identity,
        supplied_identities,
    } = prepared;
    if has_prepared_effect || has_prepared_auto_identity {
        mutation.state.prepare_writer()?;
        persist_auto_increment_identity(
            mutation.identities,
            &stmt.table,
            auto_id_col.as_deref(),
            "persist INSERT SELECT identity",
        )?;
    }
    drop(conflict_locks);
    let observed = supplied_identities.observe(mutation.publication.storage)?;
    let cancel = context.query.source.relational.runtime.cancellation_token();
    let apply_reader = prepared_rows
        .read_rows()
        .map_err(crate::physical::physical_exec_error)?;
    let mut publication =
        statement_end::publication_batch(statement_commands).with_published(published);
    let mut known_new = KnownNewInserts::new(
        preparation.referential.constraints.catalog,
        &id_column,
        stmt.on_conflict.is_some(),
    );
    for prepared_row in apply_reader {
        cancel.check()?;
        let prepared_row = prepared_row.map_err(crate::physical::physical_exec_error)?;
        let PreparedInsertSpillRow {
            target_table,
            document,
            conflict: prepared,
        } = decode_prepared_insert_spill_row(prepared_row)?;
        let inserted = known_new.identity(
            &target_table,
            &prepared,
            &observed,
            mutation.publication.identifiers,
        )?;
        apply_validated_prepared_insert(
            mutation.publication,
            &target_table,
            document,
            prepared,
            inserted,
            &mut publication,
        )?;
    }
    finish_mutation_publication(mutation.publication, &mut publication)?;
    statement_end::note_written_rows(statement_commands, &mut publication);
    statement_end::end_command(
        statement_commands,
        &triggers,
        &insert_statement_events(&stmt.table, true, conflict_update_columns),
        events.into_after_rows(),
    )?;
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            preparation.returning,
            DmlReturningShape {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: scope,
                supplemental_schema: None,
            },
            returning_rows,
            affected,
        );
    }
    Ok(MutationOutput::from_affected(affected))
}
