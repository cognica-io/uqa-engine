//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joined UPDATE execution using independent read and write statement generations.
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::MutationStatementContext;
use crate::mutation::{
    candidate::PhysicalMutationLockTarget,
    command_scope::MutationOverlayScope,
    expressions::MutationPredicate,
    prepared::{
        decode_prepared_mutation_action_row, prepared_mutation_action_schema,
        push_prepared_mutation_action, PreparedMutationAction,
    },
    returning::DmlReturningShape,
    rows::join_rows as dml_join_rows,
    statement_end,
    triggers::queue::StatementEvent,
};
use crate::query::{scope::StatementCommands, CteScope};
use uqa_sql::{
    plan::{SourcePlan, UpdatePlan},
    semantics::returning::validate_returning_alias_relations,
    SQLError, SQLParam,
};

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn run_update_from<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    read_context: &MutationStatementContext<'_, S>,
    stmt: &UpdatePlan,
    from_clause: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    statement_commands: &StatementCommands,
) -> Result<MutationOutput, SQLError> {
    let from_rows = crate::query::sources::build_join_spill_with_ctes(
        &read_context.query.source,
        from_clause,
        params,
        ctes,
    )?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(from_rows.row_schema()),
    )?;
    let cancel = context.query.source.relational.runtime.cancellation_token();
    let mut affected = 0u64;
    let mut returning_rows =
        crate::mutation::returning::ReturningRows::new(context.query.source.relational.runtime)?;
    let target = stmt.table.clone();
    let assigned_columns = stmt
        .assignments
        .iter()
        .flat_map(|assignment| assignment.target.column_names())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let statement_relation = crate::mutation::constraints::statement_relation(
        context.mutation.preparation.referential.constraints,
        &target,
    )?;
    let statement = crate::mutation::constraints::ConstraintStatement::new(
        &statement_relation,
        &assigned_columns,
    );
    let target_tables = read_context
        .mutation
        .preparation
        .referential
        .constraints
        .catalog
        .hierarchy_scan_tables(&target, stmt.include_descendants)?;
    let work_mem = context.mutation.rules.rules.memory.work_mem_bytes()?;
    let mut target_rows =
        crate::mutation::candidate_ids::CandidateIds::new(work_mem, cancel.clone());
    for (table_index, table) in target_tables.iter().enumerate() {
        let tables = read_context.query.source.scans.tables;
        let source = tables.table(table)?;
        let changes = tables.command_overlay_changes(table)?.unwrap_or_default();
        target_rows.capture_table(table_index, source.read_documents().as_ref(), &changes)?;
    }
    let target_rows = target_rows.finish()?;
    let snapshot_ctes = ctes.returning_statement_snapshot_scope_with_control(
        &context.query.source.ctes.production_resources()?.control(),
    )?;
    let mut qualification = MutationPredicate::new(
        read_context
            .mutation
            .preparation
            .referential
            .assignment
            .expressions,
        &snapshot_ctes,
        stmt.predicate.as_ref(),
        params,
    );
    let overlay = MutationOverlayScope::new(context.mutation.state);
    let action_schema = prepared_mutation_action_schema();
    let mut prepared_updates = crate::SpillBuffer::new(work_mem);
    let mut events = crate::mutation::events::MutationEventQueue::default();
    let mut completed_ids =
        super::locked_rows::LockedRows::new(work_mem, cancel.clone(), "UPDATE locked rows");
    for candidate_id in target_rows.read_rows()? {
        let (table_index, doc_id) = candidate_id?;
        let storage_table = target_tables
            .get(table_index)
            .ok_or_else(|| {
                SQLError::Internal("mutation candidate has an invalid table ordinal".into())
            })?
            .clone();
        cancel.check()?;
        let Some(candidate) = read_context
            .mutation
            .preparation
            .referential
            .constraints
            .reads
            .raw_document(&storage_table, doc_id)?
        else {
            continue;
        };
        let candidate_row = crate::mutation::rows::target_row_for_storage(
            read_context
                .mutation
                .preparation
                .referential
                .assignment
                .rows,
            &target,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &candidate,
        )?;
        for candidate_source in from_rows
            .read_rows()
            .map_err(crate::physical::physical_exec_error)?
        {
            let candidate_source =
                candidate_source.map_err(crate::physical::physical_exec_error)?;
            if !update_join_qualifies(&mut qualification, &candidate_row, &candidate_source)? {
                continue;
            }
            let projected = super::projection::project_update_row(
                read_context.mutation.preparation.referential.assignment,
                stmt,
                params,
                &snapshot_ctes,
                candidate.clone(),
                &dml_join_rows(&candidate_row, &candidate_source),
            )?;
            let PhysicalMutationLockTarget::Present { identity, recheck } =
                crate::mutation::locking::lock_physical_mutation_target(
                    context.mutation.preparation.referential.locking.session,
                    &storage_table,
                    &stmt.target_qualifier,
                    doc_id,
                    crate::query::locking::context::update_lock_strength(
                        context.mutation.preparation.referential.locking.catalog,
                        &storage_table,
                        &assigned_columns,
                    ),
                )?
            else {
                continue;
            };
            // Another command of the statement already wrote the row, which `PostgreSQL`'s `ExecUpdate` skips as `TM_SelfModified`.
            if ctes.statement_wrote(&identity) {
                continue;
            }
            // A statement that the command's triggers or functions started wrote the row under a later command id.
            if ctes.statement_triggered_write(&identity) {
                return Err(crate::mutation::errors::triggered_modification_error(
                    "updated",
                ));
            }
            let storage_table = identity.table;
            let doc_id = identity.doc_id;
            if completed_ids.contains(&storage_table, doc_id)? {
                continue;
            }
            if recheck {
                context
                    .mutation
                    .preparation
                    .referential
                    .constraints
                    .transactions
                    .refresh_explicit_statement_snapshot()?;
            }
            let Some(original_doc) = context
                .mutation
                .preparation
                .referential
                .locking
                .rows
                .get_document_for_mutation(&storage_table, doc_id)?
            else {
                continue;
            };
            let doc = if recheck {
                let target_row = crate::mutation::rows::target_row_for_storage(
                    context.mutation.preparation.referential.assignment.rows,
                    &target,
                    &storage_table,
                    &stmt.target_qualifier,
                    doc_id,
                    &original_doc,
                )?;
                if !update_join_qualifies(&mut qualification, &target_row, &candidate_source)? {
                    continue;
                }
                super::projection::project_update_row(
                    read_context.mutation.preparation.referential.assignment,
                    stmt,
                    params,
                    &snapshot_ctes,
                    original_doc.clone(),
                    &dml_join_rows(&target_row, &candidate_source),
                )?
            } else {
                projected
            };
            let source_context = candidate_source;

            if let Some(prepared) = super::prepare_update_row(
                context.mutation.preparation,
                stmt,
                params,
                &snapshot_ctes,
                statement,
                &storage_table,
                doc_id,
                original_doc,
                doc,
                Some(&source_context),
            )? {
                completed_ids.insert(&storage_table, doc_id)?;
                if let Some(returning) = prepared.returning {
                    returning_rows.push_to(
                        returning,
                        context.mutation.preparation.returning.destination,
                    )?;
                }
                affected += u64::from(prepared.affected);
                events.append_after_rows(
                    prepared.after_row_events,
                    &context.mutation.preparation.referential.triggers,
                )?;
                push_prepared_mutation_action(
                    &mut prepared_updates,
                    &action_schema,
                    PreparedMutationAction::Rewrite(prepared.rewrite),
                )?;
            }
        }
    }
    let published = overlay.finish();
    let update_statement = StatementEvent::new(
        &target,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    );
    if prepared_updates.rows() != 0 {
        context.mutation.state.prepare_writer()?;
        let mut publication =
            statement_end::publication_batch(statement_commands).with_published(published);
        for row in prepared_updates
            .drain_rows()
            .map_err(crate::physical::physical_exec_error)?
        {
            cancel.check()?;
            let action = decode_prepared_mutation_action_row(
                row.map_err(crate::physical::physical_exec_error)?,
            )?;
            crate::mutation::publication::publish_prepared_mutation_action(
                context.mutation.publication,
                action,
                crate::mutation::publication::InsertedIdentity::Unknown,
                &mut publication,
            )?;
        }
        crate::mutation::publication::finish_mutation_publication(
            context.mutation.publication,
            &mut publication,
        )?;
        statement_end::note_written_rows(statement_commands, &mut publication);
    }
    statement_end::end_command(
        statement_commands,
        &context.mutation.preparation.referential.triggers,
        std::slice::from_ref(&update_statement),
        events.into_after_rows(),
    )?;
    if !stmt.returning.is_empty() {
        let shape = DmlReturningShape {
            table: &target,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes,
            supplemental_schema: Some(from_rows.row_schema()),
        };

        return crate::mutation::returning::dml_returning_result(
            context.mutation.preparation.returning,
            shape,
            returning_rows,
            affected,
        );
    }
    Ok(MutationOutput::from_affected(affected))
}

/// A joined UPDATE projects every qualifying input before tuple admission, including inputs whose target was already changed. A tuple recheck projects again only after the replacement row qualifies.
fn update_join_qualifies<S: Clone + Send + Sync + 'static>(
    qualification: &mut MutationPredicate<'_, S>,
    target_row: &crate::OwnedPhysicalRow,
    source_context: &crate::OwnedPhysicalRow,
) -> Result<bool, SQLError> {
    let joined = dml_join_rows(target_row, source_context);
    qualification.qualifies(&joined)
}

#[cfg(test)]
mod tests;
