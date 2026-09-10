//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table UPDATE execution, with read snapshots selected after statement trigger dispatch.
use crate::mutation::{
    assignment::MutationAssignmentTarget,
    candidate::{MutationRewriteCandidate, PhysicalDocumentIdentity, PhysicalMutationLockTarget},
    command_scope::MutationOverlayScope,
    prepared::PreparedMutationAction,
    publication::MutationPublicationBatch,
    returning::DmlReturningShape,
};
use crate::query::{
    statement::context::{with_statement_snapshot, StatementContext},
    CteScope,
};
use std::collections::BTreeSet;
use uqa_sql::{
    plan::UpdatePlan,
    semantics::{
        mutation_qualifiers::validate_dml_expression_qualifiers,
        returning::validate_returning_alias_relations,
    },
    SQLError, SQLParam, SQLResult,
};
#[expect(
    clippy::too_many_lines,
    reason = "preserves statement trigger, snapshot, row mutation, and publication order"
)]
pub fn run_table_update<S: Clone + Send + Sync + 'static>(
    context: &StatementContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    let _transition_capture_scope = crate::mutation::triggers::TransitionCaptureScope::enter();
    context.source.locking.session.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.mutation.rules.rules.analysis.rules,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Update,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        uqa_sql::semantics::rules::validate_rule_returning_contract(
            context.mutation.rules.rules.analysis.rules,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Update,
            !view_returning.returning.is_empty(),
        )?;
    }
    if stmt.view_rule_update_plans.is_empty() {
        uqa_sql::assignment::columns::validate_mutation_columns(
            context.mutation.preparation.referential.assignment.columns,
            &stmt.table,
            stmt.assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            "UPDATE",
        )?;
    }
    let privilege_expressions =
        uqa_sql::semantics::mutation_privileges::ensure_update_target_privileges(
            context.mutation.privileges,
            stmt,
        )?;
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
        .rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Update)?;
    let has_update_rules = !update_rules.is_empty();
    let has_view_update_rules = !stmt.view_rule_relations.is_empty();
    let has_any_update_rules = has_update_rules || has_view_update_rules;
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
    let has_before_statement_trigger = update_original_query
        && !context
            .mutation
            .preparation
            .referential
            .triggers
            .catalog
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                false,
                &assigned_columns,
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
    if update_original_query && !has_any_update_rules {
        crate::mutation::triggers::fire_statement_triggers(
            &context.mutation.preparation.referential.triggers,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let execute_read = |read_context: &StatementContext<'_, S>| -> Result<SQLResult, SQLError> {
        let mut ctes = read_context.mutation.scopes.command_scope(
            stmt.statement_privilege_subject.as_deref(),
            stmt.relations_bound,
        )?;
        if let Some(parent) = inherited_ctes {
            ctes.inherit_cte_bindings(parent);
        }
        ctes.set_command_cte_snapshot(statement_snapshot.clone());
        crate::query::cte::materialize_plan_ctes(
            context.source.ctes,
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

        if stmt.source.is_none() {
            let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
            if let Some(predicate) = stmt.predicate.as_ref() {
                validate_dml_expression_qualifiers(predicate, &allowed)?;
            }
            for assignment in &stmt.assignments {
                validate_dml_expression_qualifiers(&assignment.value, &allowed)?;
            }
        }

        // UPDATE ... FROM other [WHERE ...]: build the joined relation,
        // evaluate WHERE against each joined row, and apply assignments to the
        // matching target rows.
        if let Some(source) = stmt.source.as_deref() {
            return super::from::run_update_from(
                context,
                read_context,
                stmt,
                source,
                params,
                &mut ctes,
            );
        }
        let row_independent_update_qualification = if has_any_update_rules {
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
        let target_tables = read_context
            .mutation
            .preparation
            .referential
            .constraints
            .catalog
            .hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
        let target_hierarchy = read_context
            .mutation
            .preparation
            .referential
            .constraints
            .partitions
            .catalog
            .try_table_hierarchy(&stmt.table)
            .map_err(|error| SQLError::Internal(format!("read UPDATE hierarchy: {error}")))?;
        let target_is_partitioned =
            target_hierarchy.partition_spec.is_some() || target_hierarchy.partition_bound.is_some();
        let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
        if !has_runtime_scope
            && target_tables.len() == 1
            && !target_is_partitioned
            && statement_snapshot.is_none()
            && !context
                .mutation
                .preparation
                .referential
                .triggers
                .catalog
                .has_row_triggers(&stmt.table, uqa_sql::ast::TriggerEvent::Update)?
            && !crate::mutation::triggers::transition_capture_required(
                &context.mutation.preparation.referential.triggers,
                &stmt.table,
                uqa_sql::ast::TriggerEvent::Update,
                &assigned_columns,
            )?
            && !context
                .mutation
                .rules
                .rules
                .analysis
                .rules
                .relation_has_rules(&stmt.table)?
            && !has_view_update_rules
        {
            if let Some(result) = crate::mutation::point_update::try_run_point_update(
                context.mutation.point_update(),
                stmt,
                params,
            )? {
                if update_original_query {
                    crate::mutation::triggers::fire_statement_triggers(
                        &context.mutation.preparation.referential.triggers,
                        &stmt.table,
                        uqa_sql::ast::TriggerTiming::After,
                        uqa_sql::ast::TriggerEvent::Update,
                        &assigned_columns,
                    )?;
                }
                return Ok(result);
            }
        }
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = context.source.relational.runtime.cancellation_token();
        // A non-volatile predicate can still use the accelerated candidate set. A VOLATILE predicate must stay in the row loop because PostgreSQL exposes each preceding logical rewrite before qualifying the next candidate.
        let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
            uqa_sql::semantics::volatility::expr_contains_volatile_function(
                context.source.volatility,
                predicate,
            )
        });
        let preselected = !has_runtime_scope && stmt.predicate.is_some() && !predicate_is_volatile;
        let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
            let filter = stmt.predicate.as_ref().ok_or_else(|| {
                SQLError::Internal("UPDATE preselection is missing its predicate".into())
            })?;
            let mut candidates = Vec::new();
            for table in &target_tables {
                candidates.extend(
                    crate::query::block::where_filter::collect_where_doc_ids(
                        &read_context.source,
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
        let overlay = MutationOverlayScope::new(context.mutation.state);
        let mut pending_updates = Vec::new();
        let mut prepared_updates = Vec::new();
        let mut events = crate::mutation::events::MutationEventQueue::default();
        let mut locked_ids = BTreeSet::new();
        for (storage_table, doc_id) in candidates {
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
                &stmt.table,
                &storage_table,
                &stmt.target_qualifier,
                doc_id,
                &candidate,
            )?;
            if !preselected {
                if let Some(filter) = stmt.predicate.as_ref() {
                    if !uqa_sql::expr::truthy(&crate::mutation::expressions::eval_mutation_expr(
                        read_context
                            .mutation
                            .preparation
                            .referential
                            .assignment
                            .expressions,
                        &snapshot_ctes,
                        filter,
                        Some(&candidate_row),
                        params,
                    )?) {
                        continue;
                    }
                }
            }
            let target = crate::mutation::locking::lock_physical_mutation_target(
                context.mutation.preparation.referential.locking.session,
                &storage_table,
                &stmt.target_qualifier,
                doc_id,
                crate::query::locking::context::update_lock_strength(
                    context.mutation.preparation.referential.locking.catalog,
                    &storage_table,
                    &assigned_columns,
                ),
            )?;
            let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
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
                &stmt.table,
                &storage_table,
                &stmt.target_qualifier,
                doc_id,
                &original_doc,
            )?;
            if recheck || preselected {
                if let Some(filter) = stmt.predicate.as_ref() {
                    if !uqa_sql::expr::truthy(&crate::mutation::expressions::eval_mutation_expr(
                        read_context
                            .mutation
                            .preparation
                            .referential
                            .assignment
                            .expressions,
                        &snapshot_ctes,
                        filter,
                        Some(&target_row),
                        params,
                    )?) {
                        continue;
                    }
                }
            }
            if evaluate_view_assignments {
                for (position, assignment) in stmt.assignments.iter().enumerate() {
                    let value = if view_original_query {
                        crate::mutation::assignment::eval_mutation_assignment(
                            read_context.mutation.preparation.referential.assignment,
                            &snapshot_ctes,
                            MutationAssignmentTarget {
                                table: &stmt.table,
                                column: &assignment.column,
                                action: "UPDATE",
                            },
                            &assignment.value,
                            Some(&target_row),
                            params,
                        )?
                    } else {
                        crate::mutation::assignment::eval_view_rule_update_assignment(
                            read_context.mutation.preparation.referential.assignment,
                            &snapshot_ctes,
                            stmt,
                            position,
                            &assignment.value,
                            Some(&target_row),
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
            if has_any_update_rules {
                pending_updates.push(MutationRewriteCandidate {
                    identity: PhysicalDocumentIdentity {
                        table: storage_table,
                        doc_id,
                    },
                    old_document: original_doc,
                    proposed_document: doc,
                    context: (),
                });
            } else if let Some(prepared) = crate::mutation::update::prepare_update_row(
                context.mutation.preparation,
                stmt,
                params,
                &snapshot_ctes,
                &assigned_columns,
                &storage_table,
                doc_id,
                original_doc,
                doc,
                events.referential_actions_mut(),
            )? {
                if let Some(returning) = prepared.returning {
                    returning_rows.push(returning);
                }
                affected += u64::from(prepared.affected);
                prepared_updates.push((
                    PreparedMutationAction::Rewrite(prepared.rewrite),
                    prepared.after_row_events,
                ));
            }
        }
        drop(overlay);
        let (view_rule_returning, rule_returning) = if has_any_update_rules {
            let rule_rows = pending_updates
                .iter()
                .map(|candidate| crate::mutation::rules::RuleRowImage {
                    old_storage_table: Some(candidate.identity.table.clone()),
                    old_doc_id: Some(candidate.identity.doc_id),
                    old: Some(candidate.old_document.clone()),
                    new_storage_table: Some(candidate.identity.table.clone()),
                    new_doc_id: Some(candidate.identity.doc_id),
                    new: Some(candidate.proposed_document.clone()),
                    context: None,
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
            view_rule_batches.configure_action_qualification(row_independent_update_qualification);
            let base_rule_indices = (0..rule_rows.len())
                .filter(|index| !view_rule_batches.suppresses(*index))
                .collect::<Vec<_>>();
            let mut rule_batch = (has_update_rules && view_original_query)
                .then(|| {
                    crate::mutation::rules::prepare_rule_batch(
                        context.mutation.rules.rules,
                        &stmt.table,
                        uqa_sql::ast::RuleEvent::Update,
                        base_rule_indices
                            .iter()
                            .filter_map(|index| rule_rows.get(*index).cloned())
                            .collect(),
                    )
                })
                .transpose()?;
            if let Some(rule_batch) = rule_batch.as_mut() {
                let count = row_independent_update_qualification
                    .unwrap_or_else(|| rule_batch.event_row_count());
                rule_batch.set_action_qualification_count(count);
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
            if update_original_query {
                crate::mutation::triggers::fire_statement_triggers(
                    &context.mutation.preparation.referential.triggers,
                    &stmt.table,
                    uqa_sql::ast::TriggerTiming::Before,
                    uqa_sql::ast::TriggerEvent::Update,
                    &assigned_columns,
                )?;
            }
            let overlay = MutationOverlayScope::new(context.mutation.state);
            for (index, candidate) in pending_updates.into_iter().enumerate() {
                if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
                    continue;
                }
                if let Some(prepared) = crate::mutation::update::prepare_update_row(
                    context.mutation.preparation,
                    stmt,
                    params,
                    &snapshot_ctes,
                    &assigned_columns,
                    &candidate.identity.table,
                    candidate.identity.doc_id,
                    candidate.old_document,
                    candidate.proposed_document,
                    events.referential_actions_mut(),
                )? {
                    if let Some(returning) = prepared.returning {
                        returning_rows.push(returning);
                    }
                    affected += u64::from(prepared.affected);
                    prepared_updates.push((
                        PreparedMutationAction::Rewrite(prepared.rewrite),
                        prepared.after_row_events,
                    ));
                }
            }
            drop(overlay);
            (view_rule_returning, rule_returning)
        } else {
            debug_assert!(pending_updates.is_empty());
            (None, None)
        };
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
                &stmt.table,
                uqa_sql::ast::TriggerEvent::Update,
                &assigned_columns,
                events.after_rows(),
            )?
        } else {
            Vec::new()
        };
        let referential_transition = events
            .referential_transition_tables(&context.mutation.preparation.referential.triggers)?;
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
                &stmt.table,
                &root_events,
                generation,
            )?;
            if update_original_query {
                crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                    &context.mutation.preparation.referential.triggers,
                    &stmt.table,
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
                    &ctes,
                    None,
                );
            }
            let shape = DmlReturningShape {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
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
        Some(snapshot) => with_statement_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
}
