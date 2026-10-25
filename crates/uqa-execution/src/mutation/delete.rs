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
use qualification::{
    count_delete_source_qualifications, qualified_delete_candidate, recheck_delete_candidate,
    DeleteCandidateQualification, DeleteCandidateRecheck,
};
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
    let delete_rules = context
        .mutation
        .rules
        .rules
        .analysis
        .rules
        .rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Delete)?;
    let has_delete_rules = !delete_rules.is_empty();
    let has_view_delete_rules = !stmt.view_rule_relations.is_empty();
    let has_any_delete_rules = has_delete_rules || has_view_delete_rules;
    let view_original_query = !stmt.view_rule_relations.iter().try_fold(
        false,
        |suppressed, relation| -> Result<bool, SQLError> {
            Ok(suppressed
                || context
                    .mutation
                    .rules
                    .rules
                    .analysis
                    .rules
                    .rules_for(relation, uqa_sql::ast::RuleEvent::Delete)?
                    .iter()
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
        },
    )?;
    let delete_original_query = view_original_query
        && !delete_rules
            .iter()
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    if delete_original_query {
        crate::mutation::routine_calls::delete(context, stmt, params, inherited_ctes)?;
    }
    let has_before_statement_trigger = delete_original_query
        && !context
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
    if delete_original_query && !has_any_delete_rules {
        statement_commands.after_triggers().fire_before_statement(
            &context.mutation.preparation.referential.triggers,
            &delete_statement,
        )?;
    }
    let mut statement_scope = None;
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<MutationOutput, SQLError> {
            let mut affected = 0u64;
            let cancel = context.query.source.relational.runtime.cancellation_token();
            let mut qualified_targets = crate::mutation::rules::RuleRowBuffer::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
            );
            let mut root_deletes = crate::mutation::row_membership::PhysicalRowBuffer::new(
                context.mutation.rules.rules.memory.work_mem_bytes()?,
                cancel.clone(),
                "deleted-row membership",
            );
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
                || {
                    uqa_sql::semantics::primary_command_cte_references(
                        &stmt.ctes,
                        &stmt.query_inputs(),
                        stmt.source_input(),
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
            let mut action_qualification_count = if has_any_delete_rules && stmt.source.is_none() {
                crate::mutation::expressions::row_independent_mutation_qualification_count(
                    read_context
                        .mutation
                        .preparation
                        .referential
                        .assignment
                        .expressions,
                    stmt.predicate.as_ref(),
                    params,
                    ctes,
                )?
            } else {
                None
            };
            if stmt.source.is_none() {
                let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
                if let Some(predicate) = stmt.predicate.as_ref() {
                    validate_dml_expression_qualifiers(predicate, &allowed)?;
                }
            }
            // DELETE FROM t USING other WHERE ... -- materialise the join
            // first, then collect target doc ids whose joined image satisfies WHERE.
            let using_rows: Option<crate::SharedSpill> = match stmt.source.as_deref() {
                Some(source) => Some(crate::query::sources::build_join_spill_with_ctes(
                    &read_context.query.source,
                    source,
                    params,
                    ctes,
                )?),
                None => None,
            };
            let qualification_references_target = if has_any_delete_rules && using_rows.is_some() {
                uqa_sql::semantics::mutation_qualifiers::qualification_references_target(
                    read_context.mutation.targets,
                    &stmt.table,
                    &stmt.target_qualifier,
                    "DELETE",
                    stmt.predicate.as_ref(),
                )?
            } else {
                false
            };
            if has_any_delete_rules {
                if let Some(using_rows) = using_rows.as_ref() {
                    action_qualification_count = Some(if qualification_references_target {
                        0
                    } else {
                        count_delete_source_qualifications(
                            read_context
                                .mutation
                                .preparation
                                .referential
                                .assignment
                                .expressions,
                            stmt,
                            ctes,
                            using_rows,
                            params,
                        )?
                    });
                }
            }
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
            let qualification_overlay =
                has_any_delete_rules.then(|| MutationOverlayScope::new(context.mutation.state));
            let overlay =
                (!has_any_delete_rules).then(|| MutationOverlayScope::new(context.mutation.state));
            let mut qualified_ids = crate::mutation::row_membership::PhysicalRowBuffer::new(
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
                let candidate = if preselected {
                    None
                } else {
                    let candidate = qualified_delete_candidate(DeleteCandidateQualification {
                        reads: read_context
                            .mutation
                            .preparation
                            .referential
                            .constraints
                            .reads,
                        rows: read_context
                            .mutation
                            .preparation
                            .referential
                            .assignment
                            .rows,
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
                        using_rows: using_rows.as_ref(),
                        doc_id,
                        count_all_qualifications: qualification_references_target,
                    })?;
                    if qualification_references_target {
                        let count = action_qualification_count.get_or_insert(0);
                        *count += candidate.qualification_count;
                    }
                    let Some(candidate) = candidate.row else {
                        continue;
                    };
                    Some(candidate)
                };
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
                    if let Some((_, Some(source_context))) = candidate.as_ref() {
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
                            source_context: Some(source_context),
                        })?
                    } else {
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
                            source_context: None,
                        })?
                    }
                } else if let Some(candidate) = candidate {
                    Some(candidate)
                } else {
                    context
                        .mutation
                        .preparation
                        .referential
                        .constraints
                        .reads
                        .get_document(&storage_table, doc_id)?
                        .map(|document| (document, None))
                };
                let Some((doc, returning_context)) = qualified else {
                    continue;
                };
                if !qualified_ids.insert_if_absent(&storage_table, doc_id)? {
                    continue;
                }
                if has_any_delete_rules {
                    qualified_targets.push(
                        crate::mutation::rules::RuleRowImage::from_delete_candidate(
                            MutationCandidate {
                                identity: PhysicalDocumentIdentity {
                                    table: storage_table,
                                    doc_id,
                                },
                                document: doc,
                                context: returning_context,
                            },
                        ),
                    )?;
                } else if crate::mutation::triggers::fire_before_row_triggers(
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
                    affected += u64::from(deleted.prepare(
                        context,
                        stmt,
                        params,
                        &snapshot_ctes,
                        MutationCandidate {
                            identity: PhysicalDocumentIdentity {
                                table: storage_table,
                                doc_id,
                            },
                            document: doc,
                            context: returning_context,
                        },
                        &|_, _| Ok(false),
                    )?);
                }
            }
            drop(qualification_overlay);
            let qualified_targets = qualified_targets.finish();
            let (view_rule_returning, rule_returning, to_delete) = if has_any_delete_rules {
                let rule_rows = &qualified_targets;
                let mut view_rule_batches =
                    crate::mutation::rules::views::prepare_view_rule_batches(
                        crate::mutation::rules::views::ViewRuleBatchRequest {
                            context: context.mutation.rules,
                            relations: &stmt.view_rule_relations,
                            event: uqa_sql::ast::RuleEvent::Delete,
                            rows: rule_rows,
                            params,
                            scope: &snapshot_ctes,
                            insert_plans: &[],
                            update_plans: &[],
                            document_relation: None,
                        },
                    )?;
                view_rule_batches.configure_action_qualification(action_qualification_count);
                let base_rule_indices = view_rule_batches.unsuppressed_indices(
                    rule_rows.len(),
                    context.mutation.rules.rules.memory.work_mem_bytes()?,
                    cancel.clone(),
                )?;
                let mut rule_batch = (has_delete_rules && view_original_query)
                    .then(|| {
                        crate::mutation::rules::prepare_retained_rule_batch(
                            context.mutation.rules.rules,
                            &stmt.table,
                            uqa_sql::ast::RuleEvent::Delete,
                            rule_rows.select_indices(
                                &base_rule_indices,
                                context.mutation.rules.rules.memory.work_mem_bytes()?,
                                cancel.clone(),
                            )?,
                        )
                    })
                    .transpose()?;
                if let Some(rule_batch) = rule_batch.as_mut() {
                    let count =
                        action_qualification_count.unwrap_or_else(|| rule_batch.event_row_count());
                    rule_batch.set_action_qualification_count(count);
                }
                let mut base_rule_suppressed =
                    crate::mutation::rules::RuleSuppression::new(rule_rows.len(), cancel.clone());
                if let Some(rule_batch) = rule_batch.as_ref() {
                    for (local_index, global_index) in base_rule_indices.reader()?.enumerate() {
                        let global_index = global_index?;
                        if rule_batch.suppresses(local_index)? {
                            base_rule_suppressed.suppress(global_index)?;
                        }
                    }
                }
                let view_rule_outcome = view_rule_batches.execute_actions_with_affected(
                    context.mutation.rules.rules,
                    stmt.view_rule_returning.as_ref(),
                )?;
                let rule_outcome = rule_batch
                    .as_ref()
                    .map(|rule_batch| {
                        rule_batch.execute_actions_with_affected(
                            context.mutation.rules.rules,
                            crate::mutation::rules::RuleReturningRequest::from_plan(
                                &stmt.returning,
                                &stmt.returning_aliases,
                                &stmt.subqueries,
                            ),
                        )
                    })
                    .transpose()?;
                if !delete_original_query {
                    affected = if view_rule_outcome.sets_command_tag {
                        view_rule_outcome.affected_rows
                    } else {
                        rule_outcome
                            .as_ref()
                            .map_or(0, |outcome| outcome.affected_rows)
                    };
                }
                let view_rule_returning = view_rule_outcome.returning;
                let rule_returning = rule_outcome.and_then(|outcome| outcome.returning);
                if view_rule_returning.is_some() && rule_returning.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "cannot have RETURNING lists in multiple rules".into(),
                    });
                }
                if delete_original_query {
                    statement_commands.after_triggers().fire_before_statement(
                        &context.mutation.preparation.referential.triggers,
                        &delete_statement,
                    )?;
                }
                let qualification_overlay = MutationOverlayScope::new(context.mutation.state);
                let mut to_delete = crate::mutation::rules::RuleIndexBuffer::new(
                    context.mutation.rules.rules.memory.work_mem_bytes()?,
                    cancel.clone(),
                )?;
                for index in 0..rule_rows.len() {
                    let suppressed = base_rule_suppressed.suppresses(index)?;
                    if view_rule_batches.suppresses(index)? || suppressed {
                        continue;
                    }
                    let candidate = qualified_targets.get(index)?.into_delete_candidate()?;
                    if crate::mutation::triggers::fire_before_row_triggers(
                        &context.mutation.preparation.referential.triggers,
                        &candidate.identity.table,
                        uqa_sql::ast::TriggerEvent::Delete,
                        candidate.identity.doc_id,
                        Some(&candidate.document),
                        None,
                        &[],
                    )?
                    .is_none()
                    {
                        continue;
                    }
                    context
                        .mutation
                        .preparation
                        .staging
                        .commands
                        .stage_command_document(
                            &candidate.identity.table,
                            candidate.identity.doc_id,
                            None,
                        )?;
                    root_deletes.insert(&candidate.identity.table, candidate.identity.doc_id)?;
                    to_delete.push(index)?;
                }
                drop(qualification_overlay);
                (view_rule_returning, rule_returning, to_delete.finish()?)
            } else {
                (
                    None,
                    None,
                    crate::mutation::rules::RuleIndices::all(
                        qualified_targets.len(),
                        cancel.clone(),
                    ),
                )
            };
            let root_deletes = root_deletes.finish()?;
            let lookup_deleted = |table: &str, doc_id| root_deletes.contains(table, doc_id);
            let overlay =
                overlay.unwrap_or_else(|| MutationOverlayScope::new(context.mutation.state));
            for index in to_delete.reader()? {
                affected += u64::from(deleted.prepare(
                    context,
                    stmt,
                    params,
                    &snapshot_ctes,
                    qualified_targets.get(index?)?.into_delete_candidate()?,
                    &lookup_deleted,
                )?);
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
                &delete_original_query
                    .then(|| delete_statement.clone())
                    .into_iter()
                    .collect::<Vec<_>>(),
                deleted.events.into_after_rows(),
            )?;
            if !stmt.returning.is_empty() {
                if let Some(view_rule_returning) = view_rule_returning {
                    return view_rule_returning.project(
                        context.mutation.preparation.returning,
                        params,
                        ctes,
                        using_rows.as_ref().map(crate::SharedSpill::row_schema),
                    );
                }
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes,
                    supplemental_schema: using_rows.as_ref().map(crate::SharedSpill::row_schema),
                };
                if let Some(rule_returning) = rule_returning {
                    return rule_returning.project(context.mutation.preparation.returning, shape);
                }
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
