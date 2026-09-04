//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE FROM join-source execution.

use super::{
    build_join_spill_with_ctes, build_returning_row, dml_join_rows, dml_returning_result,
    dml_target_row_for_storage, eval_mutation_assignment, eval_mutation_expr,
    eval_view_rule_update_assignment, finish_mutation_publication, lock_physical_mutation_target,
    prepare_partition_update_route, prepare_routed_document_rewrite,
    stage_prepared_document_rewrite, update_lock_strength, validate_returning_alias_relations,
    validate_view_checks, CteScope, DmlReturningShape, Engine, MutationAssignmentTarget,
    MutationOverlayScope, MutationPublicationBatch, MutationRewriteCandidate, MutationRowImage,
    MutationRowImages, PhysicalDocumentIdentity, PhysicalMutationLockTarget,
    PreparedMutationAction, ReturningProjectionRow, SQLError, SQLParam, SQLResult, SourcePlan,
    UpdatePlan, ViewCheckContext,
};

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(in crate::sql) fn run_update_from(
    engine: &Engine,
    read_engine: &Engine,
    stmt: &UpdatePlan,
    from_clause: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    let from_rows = build_join_spill_with_ctes(read_engine, from_clause, params, ctes)?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(from_rows.row_schema()),
    )?;
    let cancel = engine.cancellation_token();
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let target = stmt.table.clone();
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    let update_rules = engine.rules_for(&target, uqa_sql::ast::RuleEvent::Update)?;
    let has_update_rules = !update_rules.is_empty();
    let view_original_query = !stmt.view_rule_relations.iter().try_fold(
        false,
        |suppressed, relation| -> Result<bool, SQLError> {
            Ok(suppressed
                || engine
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
        || crate::sql::rules::surviving_view_rules_reference_row(
            engine,
            &stmt.view_rule_relations,
            uqa_sql::ast::RuleEvent::Update,
        )?;
    let target_tables = read_engine.hierarchy_scan_tables(&target, stmt.include_descendants)?;
    let mut target_rows = Vec::new();
    for table in target_tables {
        target_rows.extend(
            read_engine
                .table_doc_ids(&table)?
                .into_iter()
                .map(|doc_id| (table.clone(), doc_id)),
        );
    }
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let qualification_references_target =
        update_qualification_references_target(read_engine, stmt, stmt.predicate.as_ref())?;
    let mut update_qualification_count = if qualification_references_target {
        0
    } else {
        count_source_qualifications(read_engine, stmt, &snapshot_ctes, &from_rows, params)?
    };
    let overlay = MutationOverlayScope::new(engine);
    let mut pending_updates = Vec::new();
    let mut locked_ids = std::collections::BTreeSet::new();
    for (storage_table, doc_id) in target_rows {
        cancel.check()?;
        let Some(candidate) = read_engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let candidate_row = dml_target_row_for_storage(
            read_engine,
            &target,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &candidate,
        )?;
        let candidate_sources = matching_update_sources(
            read_engine,
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
            lock_physical_mutation_target(
                engine,
                &storage_table,
                &stmt.target_qualifier,
                doc_id,
                update_lock_strength(engine, &storage_table, &assigned_columns),
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
            engine.refresh_explicit_statement_snapshot()?;
        }
        let Some(mut doc) = engine.get_document_for_mutation(&storage_table, doc_id)? else {
            continue;
        };
        let original_doc = doc.clone();
        let target_row = dml_target_row_for_storage(
            engine,
            &target,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &original_doc,
        )?;
        let source_context = if recheck {
            update_join_qualifies(
                read_engine,
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
                    eval_mutation_assignment(
                        read_engine,
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
                    eval_view_rule_update_assignment(
                        read_engine,
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
        .map(|candidate| crate::sql::rules::RuleRowImage {
            old_storage_table: Some(candidate.identity.table.clone()),
            old_doc_id: Some(candidate.identity.doc_id),
            old: Some(candidate.old_document.clone()),
            new_storage_table: Some(candidate.identity.table.clone()),
            new_doc_id: Some(candidate.identity.doc_id),
            new: Some(candidate.proposed_document.clone()),
            context: Some(candidate.context.clone()),
        })
        .collect::<Vec<_>>();
    let mut view_rule_batches = super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
        engine,
        relations: &stmt.view_rule_relations,
        event: uqa_sql::ast::RuleEvent::Update,
        rows: &rule_rows,
        params,
        scope: &snapshot_ctes,
        insert_plans: &[],
        update_plans: &stmt.view_rule_update_plans,
        document_relation: None,
    })?;
    view_rule_batches.configure_action_qualification(Some(update_qualification_count));
    let base_rule_indices = (0..rule_rows.len())
        .filter(|index| !view_rule_batches.suppresses(*index))
        .collect::<Vec<_>>();
    let mut rule_batch = (has_update_rules && view_original_query)
        .then(|| {
            crate::sql::rules::prepare_rule_batch(
                engine,
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
    let view_rule_returning =
        view_rule_batches.execute_actions(engine, stmt.view_rule_returning.as_ref())?;
    let rule_returning = rule_batch
        .as_ref()
        .map(|rule_batch| {
            rule_batch.execute_actions(
                engine,
                crate::sql::rules::RuleReturningRequest::from_plan(
                    &stmt.returning,
                    &stmt.returning_aliases,
                    &stmt.subqueries,
                ),
            )
        })
        .transpose()?
        .flatten();
    if view_rule_returning.is_some() && rule_returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if (!update_rules.is_empty() || !stmt.view_rule_relations.is_empty()) && update_original_query {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let overlay = MutationOverlayScope::new(engine);
    let mut prepared_updates = Vec::new();
    let mut events = super::MutationEventQueue::default();
    for (index, candidate) in pending_updates.into_iter().enumerate() {
        if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
            continue;
        }
        let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
            engine,
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
        let Some(route) = prepare_partition_update_route(
            engine,
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
        if let Some(mut prepared) = prepare_routed_document_rewrite(
            engine,
            &candidate.identity.table,
            candidate.identity.doc_id,
            candidate.old_document,
            route,
            params,
            events.referential_actions_mut(),
        )? {
            let row_affected = !prepared.is_partition_move_delete();
            let primary_key_doc_id =
                super::integer_primary_key_doc_id(engine, &stmt.table, &prepared.new_document)?;
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
            super::validate_key_constraints(
                engine,
                &rewritten_storage_table,
                &prepared.new_document,
                (rewritten_storage_table == prepared.table).then_some(prepared.doc_id),
            )?;
            validate_view_checks(ViewCheckContext {
                engine,
                table: &stmt.table,
                storage_table: &rewritten_storage_table,
                target_qualifier: &stmt.target_qualifier,
                doc_id: rewritten_doc_id,
                document: &prepared.new_document,
                checks: &stmt.view_checks,
                params,
                scope: &snapshot_ctes,
            })?;
            let old_metadata =
                super::existing_tuple_metadata(engine, &prepared.table, prepared.doc_id)?;
            let new_metadata = super::new_tuple_metadata(engine)?;
            let mut after_row_events = Vec::new();
            let rewritten_doc_id = stage_prepared_document_rewrite(
                engine,
                &mut prepared,
                params,
                Some(&assigned_columns),
                &mut after_row_events,
            )?;
            if row_affected && !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
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
        engine.prepare_explicit_transaction_writer()?;
        let mut publication = MutationPublicationBatch::default();
        for (action, after_rows) in prepared_updates {
            super::publish_prepared_mutation_action(engine, action, false, &mut publication)?;
            events.append_after_rows(after_rows);
        }
        finish_mutation_publication(engine, &mut publication)?;
    }
    let transition_tables = if update_original_query {
        crate::sql::triggers::build_transition_tables(
            engine,
            &target,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(engine)?;
    let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
    transition_refs.extend(referential_transition.iter());
    let root_events = update_original_query
        .then_some(uqa_sql::ast::TriggerEvent::Update)
        .into_iter()
        .collect::<Vec<_>>();
    for generation in crate::sql::triggers::after_trigger_generations(&transition_refs) {
        crate::sql::triggers::fire_after_row_trigger_events_for_generation(
            engine,
            events.after_rows(),
            &transition_refs,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            engine,
            &referential_transition,
            &target,
            &root_events,
            generation,
        )?;
        if update_original_query {
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
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
            return view_rule_returning.project(engine, params, ctes, Some(from_rows.row_schema()));
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
            return rule_returning.project(engine, shape);
        }
        return dml_returning_result(engine, shape, returning_rows, affected);
    }
    Ok(SQLResult::from_affected(affected))
}

struct MatchingUpdateSources {
    first: Option<uqa_execution::OwnedPhysicalRow>,
    qualification_count: usize,
}

fn matching_update_sources(
    engine: &Engine,
    stmt: &UpdatePlan,
    ctes: &CteScope,
    from_rows: &uqa_execution::SharedSpill,
    target_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<MatchingUpdateSources, SQLError> {
    let from_reader = from_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    let mut first = None;
    let mut qualification_count = 0;
    for from_row in from_reader {
        let source_context = from_row.map_err(crate::sql::select::physical_exec_error)?;
        if update_join_qualifies(engine, stmt, ctes, target_row, &source_context, params)? {
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

fn update_qualification_references_target(
    engine: &Engine,
    stmt: &UpdatePlan,
    predicate: Option<&uqa_execution::ScalarExpr>,
) -> Result<bool, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(false);
    };
    if crate::sql::select::expr_contains_subquery(predicate) {
        return Ok(true);
    }
    let qualifiers = crate::sql::select::expr_qualifiers(predicate);
    if qualifiers.iter().any(|qualifier| {
        qualifier.eq_ignore_ascii_case(&stmt.target_qualifier)
            || qualifier.eq_ignore_ascii_case(&stmt.table)
    }) {
        return Ok(true);
    }
    if !crate::sql::select::expr_has_unqualified_column(predicate) {
        return Ok(false);
    }
    let mut columns = std::collections::BTreeSet::new();
    if !predicate.collect_columns(&mut columns) {
        return Ok(true);
    }
    let target_columns = engine
        .try_query_table_columns(&stmt.table)
        .map_err(|error| SQLError::Internal(format!("read UPDATE target columns: {error}")))?
        .into_iter()
        .chain([
            super::DOC_ID_COLUMN.to_string(),
            super::TABLE_OID_COLUMN.to_string(),
            super::XMIN_COLUMN.to_string(),
        ])
        .collect::<std::collections::BTreeSet<_>>();
    Ok(!columns.is_disjoint(&target_columns))
}

fn count_source_qualifications(
    engine: &Engine,
    stmt: &UpdatePlan,
    ctes: &CteScope,
    from_rows: &uqa_execution::SharedSpill,
    params: &[SQLParam],
) -> Result<usize, SQLError> {
    let mut count = 0;
    for source in from_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?
    {
        let source = source.map_err(crate::sql::select::physical_exec_error)?;
        let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(&source), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        count += usize::from(qualifies);
    }
    Ok(count)
}

fn update_join_qualifies(
    engine: &Engine,
    stmt: &UpdatePlan,
    ctes: &CteScope,
    target_row: &uqa_execution::OwnedPhysicalRow,
    source_context: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let joined = dml_join_rows(target_row, source_context);
    stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
            .map(|value| uqa_sql::expr::truthy(&value))
    })
}
