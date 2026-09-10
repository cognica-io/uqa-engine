//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution with snapshot qualification, tuple rechecks, and deferred publication.
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::mutation::{
    candidate::{MutationCandidate, PhysicalDocumentIdentity, PhysicalMutationLockTarget},
    command_scope::MutationOverlayScope,
    prepared::PreparedMutationAction,
    publication::MutationPublicationBatch,
    returning::{DmlReturningShape, ReturningProjectionRow},
    row_images::{MutationRowImage, MutationRowImages},
};
use crate::query::CteScope;
use std::collections::BTreeSet;
use uqa_core::DocId;
use uqa_sql::{
    plan::DeletePlan,
    semantics::{
        mutation_qualifiers::validate_dml_expression_qualifiers,
        returning::validate_returning_alias_relations,
    },
    SQLError, SQLParam, SQLResult,
};
mod qualification;
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
) -> Result<SQLResult, SQLError> {
    let privilege_subject = stmt
        .target_privilege_subject
        .clone()
        .unwrap_or_else(|| context.mutation.privileges.current_user_name());
    context.mutation.privileges.ensure_table_privilege_for(
        &stmt.table,
        &privilege_subject,
        uqa_sql::catalog::security::table::TableAclPrivilege::Delete,
    )?;
    let privilege_expressions = stmt
        .predicate
        .iter()
        .chain(stmt.returning.iter().map(|projection| &projection.expr))
        .collect::<Vec<_>>();
    context.mutation.privileges.ensure_target_select(
        uqa_sql::semantics::privileges::TargetSelectPrivilegeRequest {
            table: &stmt.table,
            privilege_subject: stmt.target_privilege_subject.as_deref(),
            target_qualifier: &stmt.target_qualifier,
            returning_aliases: &stmt.returning_aliases,
            expressions: &privilege_expressions,
            subqueries: &stmt.subqueries,
            required_columns: &[],
        },
    )?;
    let _transition_capture_scope = crate::mutation::triggers::TransitionCaptureScope::enter();
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
    if delete_original_query && !has_any_delete_rules {
        crate::mutation::triggers::fire_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<SQLResult, SQLError> {
            let mut affected = 0u64;
            let cancel = context.query.source.relational.runtime.cancellation_token();
            let mut qualified_targets: Vec<MutationCandidate<Option<crate::OwnedPhysicalRow>>> =
                Vec::new();
            let mut returning_rows = Vec::new();
            let mut ctes = read_context.mutation.scopes.command_scope(
                stmt.statement_privilege_subject.as_deref(),
                stmt.relations_bound,
            )?;
            if let Some(parent) = inherited_ctes {
                ctes.inherit_cte_bindings(parent);
            }
            ctes.set_command_cte_snapshot(statement_snapshot.clone());
            crate::query::cte::materialize_plan_ctes(
                context.query.source.ctes,
                &stmt.ctes,
                params,
                &mut ctes,
            )?;
            ctes.scalar_subqueries.clone_from(&stmt.subqueries);
            if let Some(source) = stmt.source.as_deref() {
                crate::query::privileges::ensure_select_privileges_for_source_expressions(
                    source,
                    &privilege_expressions,
                    &ctes,
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
                    &ctes,
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
                    &mut ctes,
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
                            &ctes,
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
            let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
                let filter = stmt.predicate.as_ref().ok_or_else(|| {
                    SQLError::Internal("DELETE preselection is missing its predicate".into())
                })?;
                let mut candidates = Vec::new();
                for table in &target_tables {
                    candidates.extend(
                        crate::query::block::where_filter::collect_where_doc_ids(
                            &read_context.query.source,
                            table,
                            &stmt.target_qualifier,
                            filter,
                            params,
                            &ctes,
                        )?
                        .into_iter()
                        .map(|doc_id| (table.clone(), doc_id)),
                    );
                }
                candidates
            } else {
                let mut candidates = Vec::new();
                for table in &target_tables {
                    candidates.extend(
                        read_context
                            .mutation
                            .preparation
                            .referential
                            .constraints
                            .reads
                            .table_doc_ids(table)?
                            .into_iter()
                            .map(|doc_id| (table.clone(), doc_id)),
                    );
                }
                candidates
            };
            let snapshot_ctes = ctes.returning_statement_snapshot_scope();
            let qualification_overlay = MutationOverlayScope::new(context.mutation.state);
            let mut qualified_ids = BTreeSet::new();
            for (storage_table, doc_id) in candidates {
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
                if !qualified_ids.insert((storage_table.clone(), doc_id)) {
                    continue;
                }
                if has_any_delete_rules {
                    qualified_targets.push(MutationCandidate {
                        identity: PhysicalDocumentIdentity {
                            table: storage_table,
                            doc_id,
                        },
                        document: doc,
                        context: returning_context,
                    });
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
                    context
                        .mutation
                        .preparation
                        .staging
                        .commands
                        .stage_command_document(&storage_table, doc_id, None)?;
                    qualified_targets.push(MutationCandidate {
                        identity: PhysicalDocumentIdentity {
                            table: storage_table,
                            doc_id,
                        },
                        document: doc,
                        context: returning_context,
                    });
                }
            }
            drop(qualification_overlay);
            let (view_rule_returning, rule_returning, to_delete) = if has_any_delete_rules {
                let rule_rows = qualified_targets
                    .iter()
                    .map(|candidate| crate::mutation::rules::RuleRowImage {
                        old_storage_table: Some(candidate.identity.table.clone()),
                        old_doc_id: Some(candidate.identity.doc_id),
                        old: Some(candidate.document.clone()),
                        new_storage_table: None,
                        new_doc_id: None,
                        new: None,
                        context: candidate.context.clone(),
                    })
                    .collect::<Vec<_>>();
                let mut view_rule_batches =
                    crate::mutation::rules::views::prepare_view_rule_batches(
                        crate::mutation::rules::views::ViewRuleBatchRequest {
                            context: context.mutation.rules,
                            relations: &stmt.view_rule_relations,
                            event: uqa_sql::ast::RuleEvent::Delete,
                            rows: &rule_rows,
                            params,
                            scope: &snapshot_ctes,
                            insert_plans: &[],
                            update_plans: &[],
                            document_relation: None,
                        },
                    )?;
                view_rule_batches.configure_action_qualification(action_qualification_count);
                let base_rule_indices = (0..rule_rows.len())
                    .filter(|index| !view_rule_batches.suppresses(*index))
                    .collect::<Vec<_>>();
                let mut rule_batch = (has_delete_rules && view_original_query)
                    .then(|| {
                        crate::mutation::rules::prepare_rule_batch(
                            context.mutation.rules.rules,
                            &stmt.table,
                            uqa_sql::ast::RuleEvent::Delete,
                            base_rule_indices
                                .iter()
                                .filter_map(|index| rule_rows.get(*index).cloned())
                                .collect(),
                        )
                    })
                    .transpose()?;
                if let Some(rule_batch) = rule_batch.as_mut() {
                    let count =
                        action_qualification_count.unwrap_or_else(|| rule_batch.event_row_count());
                    rule_batch.set_action_qualification_count(count);
                }
                let mut base_rule_suppressed = vec![false; rule_rows.len()];
                if let Some(rule_batch) = rule_batch.as_ref() {
                    for (local_index, global_index) in base_rule_indices.iter().copied().enumerate()
                    {
                        base_rule_suppressed[global_index] = rule_batch.suppresses(local_index);
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
                    crate::mutation::triggers::fire_statement_triggers(
                        &context.mutation.preparation.referential.triggers,
                        &stmt.table,
                        uqa_sql::ast::TriggerTiming::Before,
                        uqa_sql::ast::TriggerEvent::Delete,
                        &[],
                    )?;
                }
                let qualification_overlay = MutationOverlayScope::new(context.mutation.state);
                let mut to_delete = Vec::with_capacity(qualified_targets.len());
                for (index, candidate) in qualified_targets.into_iter().enumerate() {
                    if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
                        continue;
                    }
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
                    to_delete.push(candidate);
                }
                drop(qualification_overlay);
                (view_rule_returning, rule_returning, to_delete)
            } else {
                (None, None, qualified_targets)
            };
            let root_deletes: BTreeSet<(String, DocId)> = to_delete
                .iter()
                .map(|candidate| (candidate.identity.table.clone(), candidate.identity.doc_id))
                .collect();
            let mut prepared_deletes = Vec::with_capacity(to_delete.len());
            let mut events = crate::mutation::events::MutationEventQueue::default();
            let overlay = MutationOverlayScope::new(context.mutation.state);
            for candidate in to_delete {
                if let Some(mut prepared) = crate::mutation::referential::prepare_document_delete(
                    &context.mutation.preparation.referential,
                    &candidate.identity.table,
                    candidate.identity.doc_id,
                    params,
                    &root_deletes,
                    events.referential_actions_mut(),
                    false,
                )? {
                    let old_metadata = crate::mutation::rows::existing_tuple_metadata(
                        context.mutation.preparation.referential.assignment.rows,
                        &prepared.table,
                        prepared.doc_id,
                    )?;
                    crate::mutation::staging::stage_prepared_document_delete(
                        context.mutation.preparation.staging,
                        &mut prepared,
                        params,
                        events.after_rows_mut(),
                    )?;
                    affected += 1;
                    if !stmt.returning.is_empty() {
                        returning_rows.push(crate::mutation::returning::build_returning_row(
                            context.mutation.preparation.returning,
                            ReturningProjectionRow {
                                table: &stmt.table,
                                target_qualifier: &stmt.target_qualifier,
                                images: MutationRowImages {
                                    old: Some(MutationRowImage {
                                        storage_table: prepared.table.clone(),
                                        doc_id: prepared.doc_id,
                                        document: &prepared.document,
                                        metadata: old_metadata,
                                    }),
                                    new: None,
                                },
                                aliases: &stmt.returning_aliases,
                                context: candidate.context.as_ref(),
                            },
                            &stmt.returning,
                            params,
                            &snapshot_ctes,
                        )?);
                    }
                    prepared_deletes.push(PreparedMutationAction::Delete(prepared));
                }
            }
            drop(overlay);
            if !prepared_deletes.is_empty() {
                context.mutation.state.prepare_writer()?;
                let mut publication = MutationPublicationBatch::default();
                for action in prepared_deletes {
                    crate::mutation::publication::publish_prepared_mutation_action(
                        context.mutation.publication,
                        action,
                        false,
                        &mut publication,
                    )?;
                }
                crate::mutation::publication::finish_mutation_publication(
                    context.mutation.publication,
                    &mut publication,
                )?;
            }
            let transition_tables = if delete_original_query {
                crate::mutation::triggers::build_transition_tables(
                    &context.mutation.preparation.referential.triggers,
                    &stmt.table,
                    uqa_sql::ast::TriggerEvent::Delete,
                    &[],
                    events.after_rows(),
                )?
            } else {
                Vec::new()
            };
            let referential_transition = events.referential_transition_tables(
                &context.mutation.preparation.referential.triggers,
            )?;
            let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
            transition_refs.extend(referential_transition.iter());
            let root_events = delete_original_query
                .then_some(uqa_sql::ast::TriggerEvent::Delete)
                .into_iter()
                .collect::<Vec<_>>();
            for generation in crate::mutation::triggers::after_trigger_generations(&transition_refs)
            {
                crate::mutation::triggers::fire_after_row_trigger_events_for_generation(
                    &context.mutation.preparation.referential.triggers,
                    events.after_rows(),
                    &transition_refs,
                    generation,
                )?;
                events.fire_referential_after_statement_triggers(
                    &context.mutation.preparation.referential.triggers,
                    &referential_transition,
                    &stmt.table,
                    &root_events,
                    generation,
                )?;
                if delete_original_query {
                    crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                        &context.mutation.preparation.referential.triggers,
                        &stmt.table,
                        uqa_sql::ast::TriggerEvent::Delete,
                        &[],
                        &transition_tables,
                        generation,
                    )?;
                }
            }
            if !stmt.returning.is_empty() {
                if let Some(view_rule_returning) = view_rule_returning {
                    return view_rule_returning.project(
                        context.mutation.preparation.returning,
                        params,
                        &ctes,
                        using_rows.as_ref().map(crate::SharedSpill::row_schema),
                    );
                }
                let shape = DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: using_rows.as_ref().map(crate::SharedSpill::row_schema),
                };
                if let Some(rule_returning) = rule_returning {
                    return rule_returning.project(context.mutation.preparation.returning, shape);
                }
                return crate::mutation::returning::dml_returning_result(
                    context.mutation.preparation.returning,
                    shape,
                    returning_rows,
                    affected,
                );
            }
            Ok(SQLResult::from_affected(affected))
        };
    match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
}
