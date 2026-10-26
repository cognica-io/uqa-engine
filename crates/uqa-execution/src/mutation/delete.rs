//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution with snapshot qualification, tuple rechecks, and deferred publication.
use crate::mutation::output::MutationOutput;
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::mutation::{
    candidate::{MutationCandidate, PhysicalDocumentIdentity, PhysicalMutationLockTarget},
    command_scope::MutationOverlayScope,
    prepared::decode_prepared_mutation_action_row,
    returning::DmlReturningShape,
    statement_end,
    triggers::queue::StatementEvent,
};
use crate::query::CteScope;
use std::collections::BTreeSet;
use uqa_sql::{
    plan::DeletePlan,
    semantics::{
        mutation_qualifiers::validate_dml_expression_qualifiers,
        returning::validate_returning_alias_relations,
    },
    SQLError, SQLParam,
};
mod qualification;
mod rows;
use qualification::{recheck_delete_candidate, DeleteCandidateRecheck, DeleteSourceRows};
#[expect(
    clippy::too_many_lines,
    reason = "preserves statement trigger, snapshot, row deletion, and publication order"
)]
pub fn run_table_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let privilege_expressions =
        uqa_sql::semantics::mutation_privileges::ensure_delete_target_privileges(
            context.mutation.privileges,
            stmt,
        )?;
    let (statement_commands, _running_statement) =
        statement_end::statement_commands(inherited_ctes);
    let _trigger_scope = crate::mutation::triggers::TriggerStatementScope::enter();
    context.query.source.locking.session.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.mutation.rules.rules.analysis.rules,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Delete,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        uqa_sql::semantics::rules::validate_rule_returning_contract(
            context.mutation.rules.rules.analysis.rules,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Delete,
            !view_returning.returning.is_empty(),
        )?;
    }
    crate::mutation::routine_calls::delete(context, stmt, params, inherited_ctes)?;
    let has_before_statement_trigger = !context
        .mutation
        .preparation
        .referential
        .triggers
        .catalog
        .triggers_for(
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            false,
            &[],
        )?
        .is_empty();
    let statement_snapshot = match inherited_ctes.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Some(snapshot),
        None if has_before_statement_trigger
            || stmt.ctes.iter().any(|cte| cte.body.modifies_data()) =>
        {
            Some(std::sync::Arc::new(context.snapshots.capture()?))
        }
        None => None,
    };
    let delete_statement =
        StatementEvent::new(&stmt.table, uqa_sql::ast::TriggerEvent::Delete, &[]);

    statement_commands.after_triggers().fire_before_statement(
        &context.mutation.preparation.referential.triggers,
        &delete_statement,
    )?;

    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<MutationOutput, SQLError> {
            let mut affected = 0u64;
            let cancel = context.query.source.relational.runtime.cancellation_token();
            let mut deleted = rows::DeleteRows::new(context)?;
            let ctes = statement_scope.insert(read_context.mutation.scopes.command_scope(
                stmt.statement_privilege_subject.as_ref(),
                stmt.relations_bound,
            )?);
            if let Some(parent) = inherited_ctes {
                ctes.inherit_cte_bindings(parent);
            }
            ctes.set_statement_commands(std::sync::Arc::clone(&statement_commands));
            ctes.set_command_cte_snapshot(statement_snapshot.clone());
            crate::query::cte::materialize_command_ctes(
                context.query.source.ctes,
                &stmt.ctes,
                |control| {
                    uqa_sql::semantics::primary_command_cte_references_with_control(
                        &stmt.ctes,
                        stmt.query_inputs(),
                        stmt.source_input(),
                        control,
                    )
                },
                params,
                ctes,
            )?;
            ctes.scalar_subqueries = stmt.subqueries.as_slice().into();
            if let Some(source) = stmt.source.as_deref() {
                crate::query::privileges::ensure_select_privileges_for_source_expressions(
                    source,
                    &privilege_expressions,
                    ctes,
                )?;
            }
            if stmt.source.is_none() {
                let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
                if let Some(predicate) = stmt.predicate.as_ref() {
                    validate_dml_expression_qualifiers(predicate, &allowed)?;
                }
            }
            // Retain the USING source, then qualify each joined row immediately before its mutation and RETURNING output.
            let using_rows: Option<crate::SharedSpill> = match stmt.source.as_deref() {
                Some(source) => Some(crate::query::sources::build_join_spill_with_ctes(
                    &read_context.query.source,
                    source,
                    params,
                    ctes,
                )?),
                None => None,
            };
            validate_returning_alias_relations(
                &stmt.target_qualifier,
                &stmt.returning_aliases,
                using_rows.as_ref().map(crate::SharedSpill::row_schema),
            )?;
            let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
            // A non-volatile plain predicate can use the accelerated candidate set. A VOLATILE predicate must qualify rows in command order so each prior logical deletion is visible to the next callback.
            let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
                uqa_sql::semantics::volatility::expr_contains_volatile_function(
                    context.query.source.volatility,
                    predicate,
                )
            });
            let preselected = !has_runtime_scope
                && stmt.source.is_none()
                && stmt.predicate.is_some()
                && !predicate_is_volatile;
            let target_tables = read_context
                .mutation
                .preparation
                .referential
                .constraints
                .catalog
                .hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
            let mut candidates = crate::mutation::candidate_ids::CandidateIds::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
            );
            for (table_index, table) in target_tables.iter().enumerate() {
                if preselected {
                    let filter = stmt.predicate.as_ref().ok_or_else(|| {
                        SQLError::Internal("DELETE preselection is missing its predicate".into())
                    })?;
                    crate::query::block::where_filter::visit_where_doc_ids(
                        &read_context.query.source,
                        table,
                        &stmt.target_qualifier,
                        filter,
                        params,
                        ctes,
                        &mut |doc_id| candidates.push(table_index, doc_id),
                    )?;
                } else {
                    let tables = read_context.query.source.scans.tables;
                    let source = tables.table(table)?;
                    let changes = tables.command_overlay_changes(table)?.unwrap_or_default();
                    candidates.capture_table(
                        table_index,
                        source.read_documents().as_ref(),
                        &changes,
                    )?;
                }
            }
            let candidates = candidates.finish()?;
            let snapshot_ctes = ctes.returning_statement_snapshot_scope();
            let mut qualification = crate::mutation::expressions::MutationPredicate::new(
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
            let mut completed_ids = crate::mutation::row_membership::PhysicalRowBuffer::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
                "deleted-row membership",
            );
            for candidate_id in candidates.read_rows()? {
                let (table_index, doc_id) = candidate_id?;
                let storage_table = target_tables
                    .get(table_index)
                    .ok_or_else(|| {
                        SQLError::Internal("mutation candidate has an invalid table ordinal".into())
                    })?
                    .clone();
                cancel.check()?;
                let Some(candidate_document) = read_context
                    .mutation
                    .preparation
                    .referential
                    .constraints
                    .reads
                    .get_document(&storage_table, doc_id)?
                else {
                    continue;
                };
                let target_row = crate::mutation::rows::target_row_for_storage(
                    read_context
                        .mutation
                        .preparation
                        .referential
                        .assignment
                        .rows,
                    &stmt.table,
                    &storage_table,
                    &stmt.target_qualifier,
                    doc_id,
                    &candidate_document,
                )?;
                for source_context in DeleteSourceRows::new(using_rows.as_ref())? {
                    let source_context = source_context?;
                    if !preselected {
                        let joined = source_context
                            .as_ref()
                            .map(|source| crate::mutation::rows::join_rows(&target_row, source));
                        if !qualification.qualifies(joined.as_ref().unwrap_or(&target_row))? {
                            continue;
                        }
                    }
                    let target = crate::mutation::locking::lock_physical_mutation_target(
                        context.mutation.preparation.referential.locking.session,
                        &storage_table,
                        &stmt.target_qualifier,
                        doc_id,
                        uqa_sql::ast::LockStrength::ForUpdate,
                    )?;
                    let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
                        continue;
                    };
                    // Another command of the statement already wrote the row, which `PostgreSQL`'s `ExecDelete` skips as `TM_SelfModified`.
                    if ctes.statement_wrote(&identity) {
                        continue;
                    }
                    // A statement that the command's triggers or functions started wrote the row under a later command id; a BEFORE ROW trigger fetches the row through `GetTupleForTrigger`, whose error names an update.
                    if ctes.statement_triggered_write(&identity) {
                        let operation = if crate::mutation::triggers::has_before_row_triggers(
                            &context.mutation.preparation.referential.triggers,
                            &identity.table,
                            uqa_sql::ast::TriggerEvent::Delete,
                        )? {
                            "updated"
                        } else {
                            "deleted"
                        };
                        return Err(crate::mutation::errors::triggered_modification_error(
                            operation,
                        ));
                    }
                    let storage_table = identity.table;
                    let doc_id = identity.doc_id;
                    let qualified = if recheck {
                        context
                            .mutation
                            .preparation
                            .referential
                            .constraints
                            .transactions
                            .refresh_explicit_statement_snapshot()?;
                        recheck_delete_candidate(DeleteCandidateRecheck {
                            reads: context.mutation.preparation.referential.constraints.reads,
                            rows: context.mutation.preparation.referential.assignment.rows,
                            expressions: read_context
                                .mutation
                                .preparation
                                .referential
                                .assignment
                                .expressions,
                            stmt,
                            storage_table: &storage_table,
                            params,
                            ctes: &snapshot_ctes,
                            doc_id,
                            source_context: source_context.as_ref(),
                        })?
                    } else {
                        Some((candidate_document.clone(), source_context))
                    };
                    let Some((doc, returning_context)) = qualified else {
                        continue;
                    };
                    if completed_ids.contains(&storage_table, doc_id)? {
                        continue;
                    }
                    if crate::mutation::triggers::fire_before_row_triggers(
                        &context.mutation.preparation.referential.triggers,
                        &storage_table,
                        uqa_sql::ast::TriggerEvent::Delete,
                        doc_id,
                        Some(&doc),
                        None,
                        &[],
                    )?
                    .is_some()
                    {
                        let removed = deleted.prepare(
                            context,
                            stmt,
                            params,
                            &snapshot_ctes,
                            MutationCandidate {
                                identity: PhysicalDocumentIdentity {
                                    table: storage_table.clone(),
                                    doc_id,
                                },
                                document: doc,
                                context: returning_context,
                            },
                            &|_, _| Ok(false),
                        )?;
                        if removed {
                            completed_ids.insert(&storage_table, doc_id)?;
                            affected += 1;
                        }
                    }
                }
            }
            let published = overlay.finish();
            if deleted.prepared.rows() != 0 {
                context.mutation.state.prepare_writer()?;
                let mut publication =
                    statement_end::publication_batch(&statement_commands).with_published(published);
                for row in deleted
                    .prepared
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
                statement_end::note_written_rows(&statement_commands, &mut publication);
            }
            statement_end::end_command(
                &statement_commands,
                &context.mutation.preparation.referential.triggers,
                std::slice::from_ref(&delete_statement),
                deleted.events.into_after_rows(),
            )?;
            if !stmt.returning.is_empty() {
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes,
                    supplemental_schema: using_rows.as_ref().map(crate::SharedSpill::row_schema),
                };

                return crate::mutation::returning::dml_returning_result(
                    context.mutation.preparation.returning,
                    shape,
                    deleted.returning,
                    affected,
                );
            }
            Ok(MutationOutput::from_affected(affected))
        };
    let result = match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    };
    statement_end::finish_statement(
        context,
        params,
        &stmt.ctes,
        statement_scope.as_mut(),
        result,
    )
}
