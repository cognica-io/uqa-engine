//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joined UPDATE execution using independent read and write statement generations.
use crate::mutation::statement::context::MutationStatementContext;
use crate::mutation::{
    assignment::{validate_view_checks, MutationAssignmentTarget, ViewCheckContext},
    candidate::{MutationRewriteCandidate, PhysicalDocumentIdentity, PhysicalMutationLockTarget},
    command_scope::MutationOverlayScope,
    prepared::PreparedMutationAction,
    publication::MutationPublicationBatch,
    returning::{DmlReturningShape, ReturningProjectionRow},
    row_images::{MutationRowImage, MutationRowImages},
    rows::join_rows as dml_join_rows,
};
use crate::query::CteScope;
use uqa_sql::{
    plan::{SourcePlan, UpdatePlan},
    semantics::returning::validate_returning_alias_relations,
    SQLError, SQLParam, SQLResult,
};

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn run_update_from<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    read_context: &MutationStatementContext<'_, S>,
    stmt: &UpdatePlan,
    from_clause: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<SQLResult, SQLError> {
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
    let mut returning_rows = Vec::new();
    let target = stmt.table.clone();
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    let update_rules = context
        .mutation
        .rules
        .rules
        .analysis
        .rules
        .rules_for(&target, uqa_sql::ast::RuleEvent::Update)?;
    let has_update_rules = !update_rules.is_empty();
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
                    .rules_for(relation, uqa_sql::ast::RuleEvent::Update)?
                    .iter()
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
        },
    )?;
    let update_original_query = view_original_query
        && !update_rules
            .iter()
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    let evaluate_view_assignments = view_original_query
        || uqa_sql::semantics::rules::analysis::surviving_view_rules_reference_row(
            context.mutation.rules.rules.analysis,
            &stmt.view_rule_relations,
            uqa_sql::ast::RuleEvent::Update,
        )?;
    let target_tables = read_context
        .mutation
        .preparation
        .referential
        .constraints
        .catalog
        .hierarchy_scan_tables(&target, stmt.include_descendants)?;
    let mut target_rows = Vec::new();
    for table in target_tables {
        target_rows.extend(
            read_context
                .mutation
                .preparation
                .referential
                .constraints
                .reads
                .table_doc_ids(&table)?
                .into_iter()
                .map(|doc_id| (table.clone(), doc_id)),
        );
    }
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let qualification_references_target =
        uqa_sql::semantics::mutation_qualifiers::qualification_references_target(
            read_context.mutation.targets,
            &stmt.table,
            &stmt.target_qualifier,
            "UPDATE",
            stmt.predicate.as_ref(),
        )?;
    let mut update_qualification_count = if qualification_references_target {
        0
    } else {
        count_source_qualifications(
            read_context
                .mutation
                .preparation
                .referential
                .assignment
                .expressions,
            stmt,
            &snapshot_ctes,
            &from_rows,
            params,
        )?
    };
    let overlay = MutationOverlayScope::new(context.mutation.state);
    let mut pending_updates = Vec::new();
    let mut locked_ids = std::collections::BTreeSet::new();
    for (storage_table, doc_id) in target_rows {
        cancel.check()?;
        let Some(candidate) = read_context
            .mutation
            .preparation
            .referential
            .constraints
            .reads
            .get_document(&storage_table, doc_id)?
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
        let candidate_sources = matching_update_sources(
            read_context
                .mutation
                .preparation
                .referential
                .assignment
                .expressions,
            stmt,
            &snapshot_ctes,
            &from_rows,
            &candidate_row,
            params,
        )?;
        if qualification_references_target {
            update_qualification_count += candidate_sources.qualification_count;
        }
        let Some(candidate_source) = candidate_sources.first else {
            continue;
        };
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
        let storage_table = identity.table;
        let doc_id = identity.doc_id;
        if !locked_ids.insert((storage_table.clone(), doc_id)) {
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
        let Some(mut doc) = context
            .mutation
            .preparation
            .referential
            .locking
            .rows
            .get_document_for_mutation(&storage_table, doc_id)?
        else {
            continue;
        };
        let original_doc = doc.clone();
        let target_row = crate::mutation::rows::target_row_for_storage(
            context.mutation.preparation.referential.assignment.rows,
            &target,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &original_doc,
        )?;
        let source_context = if recheck {
            update_join_qualifies(
                read_context
                    .mutation
                    .preparation
                    .referential
                    .assignment
                    .expressions,
                stmt,
                &snapshot_ctes,
                &target_row,
                &candidate_source,
                params,
            )?
            .then_some(candidate_source)
        } else {
            Some(candidate_source)
        };
        let Some(source_context) = source_context else {
            continue;
        };
        let joined = dml_join_rows(&target_row, &source_context);
        if evaluate_view_assignments {
            // Apply assignments evaluated against the rechecked joined row so RHS expressions cannot consume a target image from before the lock wait.
            for (position, assignment) in stmt.assignments.iter().enumerate() {
                let value = if view_original_query {
                    crate::mutation::assignment::eval_mutation_assignment(
                        read_context.mutation.preparation.referential.assignment,
                        &snapshot_ctes,
                        MutationAssignmentTarget {
                            table: &target,
                            column: &assignment.column,
                            action: "UPDATE FROM",
                        },
                        &assignment.value,
                        Some(&joined),
                        params,
                    )?
                } else {
                    crate::mutation::assignment::eval_view_rule_update_assignment(
                        read_context.mutation.preparation.referential.assignment,
                        &snapshot_ctes,
                        stmt,
                        position,
                        &assignment.value,
                        Some(&joined),
                        params,
                    )?
                };
                if let Some(value) = value {
                    doc.insert(assignment.column.clone(), value);
                } else {
                    doc.remove(&assignment.column);
                }
            }
        }
        pending_updates.push(MutationRewriteCandidate {
            identity: PhysicalDocumentIdentity {
                table: storage_table,
                doc_id,
            },
            old_document: original_doc,
            proposed_document: doc,
            context: source_context,
        });
    }
    drop(overlay);
    let rule_rows = pending_updates
        .iter()
        .map(|candidate| crate::mutation::rules::RuleRowImage {
            old_storage_table: Some(candidate.identity.table.clone()),
            old_doc_id: Some(candidate.identity.doc_id),
            old: Some(candidate.old_document.clone()),
            new_storage_table: Some(candidate.identity.table.clone()),
            new_doc_id: Some(candidate.identity.doc_id),
            new: Some(candidate.proposed_document.clone()),
            context: Some(candidate.context.clone()),
        })
        .collect::<Vec<_>>();
    let mut view_rule_batches = crate::mutation::rules::views::prepare_view_rule_batches(
        crate::mutation::rules::views::ViewRuleBatchRequest {
            context: context.mutation.rules,
            relations: &stmt.view_rule_relations,
            event: uqa_sql::ast::RuleEvent::Update,
            rows: &rule_rows,
            params,
            scope: &snapshot_ctes,
            insert_plans: &[],
            update_plans: &stmt.view_rule_update_plans,
            document_relation: None,
        },
    )?;
    view_rule_batches.configure_action_qualification(Some(update_qualification_count));
    let base_rule_indices = (0..rule_rows.len())
        .filter(|index| !view_rule_batches.suppresses(*index))
        .collect::<Vec<_>>();
    let mut rule_batch = (has_update_rules && view_original_query)
        .then(|| {
            crate::mutation::rules::prepare_rule_batch(
                context.mutation.rules.rules,
                &target,
                uqa_sql::ast::RuleEvent::Update,
                base_rule_indices
                    .iter()
                    .filter_map(|index| rule_rows.get(*index).cloned())
                    .collect(),
            )
        })
        .transpose()?;
    if let Some(rule_batch) = rule_batch.as_mut() {
        rule_batch.set_action_qualification_count(update_qualification_count);
    }
    let mut base_rule_suppressed = vec![false; rule_rows.len()];
    if let Some(rule_batch) = rule_batch.as_ref() {
        for (local_index, global_index) in base_rule_indices.iter().copied().enumerate() {
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
    if !update_original_query {
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
    if (!update_rules.is_empty() || !stmt.view_rule_relations.is_empty()) && update_original_query {
        crate::mutation::triggers::fire_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &target,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let overlay = MutationOverlayScope::new(context.mutation.state);
    let mut prepared_updates = Vec::new();
    let mut events = crate::mutation::events::MutationEventQueue::default();
    for (index, candidate) in pending_updates.into_iter().enumerate() {
        if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
            continue;
        }
        let Some(triggered_document) = crate::mutation::triggers::fire_before_row_triggers(
            &context.mutation.preparation.referential.triggers,
            &candidate.identity.table,
            uqa_sql::ast::TriggerEvent::Update,
            candidate.identity.doc_id,
            Some(&candidate.old_document),
            Some(&candidate.proposed_document),
            &assigned_columns,
        )?
        else {
            continue;
        };
        let Some(route) = crate::mutation::referential::prepare_partition_update_route(
            &context.mutation.preparation.referential,
            &candidate.identity.table,
            candidate.identity.doc_id,
            &candidate.old_document,
            triggered_document,
            &target,
            params,
            stmt.include_descendants,
        )?
        else {
            continue;
        };
        if let Some(mut prepared) = crate::mutation::referential::prepare_routed_document_rewrite(
            &context.mutation.preparation.referential,
            &candidate.identity.table,
            candidate.identity.doc_id,
            candidate.old_document,
            route,
            params,
            events.referential_actions_mut(),
        )? {
            let row_affected = !prepared.is_partition_move_delete();
            let primary_key_doc_id = crate::mutation::identity::integer_primary_key_doc_id(
                context.mutation.preparation.referential.constraints.catalog,
                &stmt.table,
                &prepared.new_document,
            )?;
            let rewritten_doc_id = prepared
                .destination
                .as_ref()
                .map(|(_, doc_id)| *doc_id)
                .or(primary_key_doc_id)
                .unwrap_or(prepared.doc_id);
            let rewritten_storage_table = prepared
                .destination
                .as_ref()
                .map_or_else(|| prepared.table.clone(), |(table, _)| table.clone());
            crate::mutation::constraints::validate_key_constraints(
                context.mutation.preparation.referential.constraints,
                &rewritten_storage_table,
                &prepared.new_document,
                (rewritten_storage_table == prepared.table).then_some(prepared.doc_id),
            )?;
            validate_view_checks(ViewCheckContext {
                services: context.mutation.preparation.referential.assignment,
                table: &stmt.table,
                storage_table: &rewritten_storage_table,
                target_qualifier: &stmt.target_qualifier,
                doc_id: rewritten_doc_id,
                document: &prepared.new_document,
                checks: &stmt.view_checks,
                params,
                scope: &snapshot_ctes,
            })?;
            let old_metadata = crate::mutation::rows::existing_tuple_metadata(
                context.mutation.preparation.referential.assignment.rows,
                &prepared.table,
                prepared.doc_id,
            )?;
            let new_metadata = crate::mutation::rows::new_tuple_metadata(
                context.mutation.preparation.referential.assignment.rows,
            )?;
            let mut after_row_events = Vec::new();
            let rewritten_doc_id = crate::mutation::staging::stage_prepared_document_rewrite(
                context.mutation.preparation.staging,
                &mut prepared,
                params,
                Some(&assigned_columns),
                &mut after_row_events,
            )?;
            if row_affected && !stmt.returning.is_empty() {
                returning_rows.push(crate::mutation::returning::build_returning_row(
                    context.mutation.preparation.returning,
                    ReturningProjectionRow {
                        table: &target,
                        target_qualifier: &stmt.target_qualifier,
                        images: MutationRowImages {
                            old: Some(MutationRowImage {
                                storage_table: prepared.table.clone(),
                                doc_id: prepared.doc_id,
                                document: &prepared.old_document,
                                metadata: old_metadata,
                            }),
                            new: Some(MutationRowImage {
                                storage_table: rewritten_storage_table,
                                doc_id: rewritten_doc_id,
                                document: &prepared.new_document,
                                metadata: new_metadata,
                            }),
                        },
                        aliases: &stmt.returning_aliases,
                        context: Some(&candidate.context),
                    },
                    &stmt.returning,
                    params,
                    &snapshot_ctes,
                )?);
            }
            affected += u64::from(row_affected);
            prepared_updates.push((PreparedMutationAction::Rewrite(prepared), after_row_events));
        }
    }
    drop(overlay);
    if !prepared_updates.is_empty() {
        context.mutation.state.prepare_writer()?;
        let mut publication = MutationPublicationBatch::default();
        for (action, after_rows) in prepared_updates {
            crate::mutation::publication::publish_prepared_mutation_action(
                context.mutation.publication,
                action,
                false,
                &mut publication,
            )?;
            events.append_after_rows(after_rows);
        }
        crate::mutation::publication::finish_mutation_publication(
            context.mutation.publication,
            &mut publication,
        )?;
    }
    let transition_tables = if update_original_query {
        crate::mutation::triggers::build_transition_tables(
            &context.mutation.preparation.referential.triggers,
            &target,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition =
        events.referential_transition_tables(&context.mutation.preparation.referential.triggers)?;
    let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
    transition_refs.extend(referential_transition.iter());
    let root_events = update_original_query
        .then_some(uqa_sql::ast::TriggerEvent::Update)
        .into_iter()
        .collect::<Vec<_>>();
    for generation in crate::mutation::triggers::after_trigger_generations(&transition_refs) {
        crate::mutation::triggers::fire_after_row_trigger_events_for_generation(
            &context.mutation.preparation.referential.triggers,
            events.after_rows(),
            &transition_refs,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &referential_transition,
            &target,
            &root_events,
            generation,
        )?;
        if update_original_query {
            crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                &context.mutation.preparation.referential.triggers,
                &target,
                uqa_sql::ast::TriggerEvent::Update,
                &assigned_columns,
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
                ctes,
                Some(from_rows.row_schema()),
            );
        }
        let shape = DmlReturningShape {
            table: &target,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes,
            supplemental_schema: Some(from_rows.row_schema()),
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
}

struct MatchingUpdateSources {
    first: Option<crate::OwnedPhysicalRow>,
    qualification_count: usize,
}

fn matching_update_sources<S: Clone + Send + Sync + 'static>(
    expressions: crate::mutation::rows::context::MutationExpressionContext<'_, S>,
    stmt: &UpdatePlan,
    ctes: &CteScope<S>,
    from_rows: &crate::SharedSpill,
    target_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<MatchingUpdateSources, SQLError> {
    let from_reader = from_rows
        .read_rows()
        .map_err(crate::query::projection::physical_exec_error)?;
    let mut first = None;
    let mut qualification_count = 0;
    for from_row in from_reader {
        let source_context = from_row.map_err(crate::query::projection::physical_exec_error)?;
        if update_join_qualifies(expressions, stmt, ctes, target_row, &source_context, params)? {
            qualification_count += 1;
            if first.is_none() {
                first = Some(source_context);
            }
        }
    }
    Ok(MatchingUpdateSources {
        first,
        qualification_count,
    })
}

fn count_source_qualifications<S: Clone + Send + Sync + 'static>(
    expressions: crate::mutation::rows::context::MutationExpressionContext<'_, S>,
    stmt: &UpdatePlan,
    ctes: &CteScope<S>,
    from_rows: &crate::SharedSpill,
    params: &[SQLParam],
) -> Result<usize, SQLError> {
    let mut count = 0;
    for source in from_rows
        .read_rows()
        .map_err(crate::query::projection::physical_exec_error)?
    {
        let source = source.map_err(crate::query::projection::physical_exec_error)?;
        let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |predicate| {
            crate::mutation::expressions::eval_mutation_expr(
                expressions,
                ctes,
                predicate,
                Some(&source),
                params,
            )
            .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        count += usize::from(qualifies);
    }
    Ok(count)
}

fn update_join_qualifies<S: Clone + Send + Sync + 'static>(
    expressions: crate::mutation::rows::context::MutationExpressionContext<'_, S>,
    stmt: &UpdatePlan,
    ctes: &CteScope<S>,
    target_row: &crate::OwnedPhysicalRow,
    source_context: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let joined = dml_join_rows(target_row, source_context);
    stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        crate::mutation::expressions::eval_mutation_expr(
            expressions,
            ctes,
            filter,
            Some(&joined),
            params,
        )
        .map(|value| uqa_sql::expr::truthy(&value))
    })
}
