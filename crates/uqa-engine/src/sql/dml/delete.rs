//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution and referenced-key delete actions.

use super::{
    apply_set_action_to_child, apply_validated_prepared_document_rewrite,
    build_join_spill_with_ctes, build_returning_row, dml_join_rows, dml_returning_result,
    dml_target_row_for_storage, eval_mutation_expr, foreign_key_comparison_types,
    foreign_key_lookup_values, lock_mutation_target, lock_physical_mutation_target,
    period_foreign_key_coverage, prepare_referential_document_rewrite, referencing_rows,
    referrers_to_for_actions, validate_dml_expression_qualifiers,
    validate_returning_alias_relations, BTreeSet, CteScope, DeletePlan, DmlCommandMutationOverlay,
    DmlReturningShape, DocId, Document, Engine, ForeignKey, ForeignKeyAction, MutationLockTarget,
    PhysicalDocumentIdentity, PhysicalMutationLockTarget, PreparedDeleteAction,
    PreparedDocumentDelete, ReferencingChildLock, ReturningProjectionRow, ReturningRowImage,
    ReturningRowImages, SQLError, SQLParam, SQLResult, Value,
};

pub(in crate::sql) fn run_delete(
    engine: &Engine,
    stmt: DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    if engine.transaction_depth() != 0 {
        run_delete_inner(engine, &stmt, params)
    } else {
        engine.transaction(move |engine| run_delete_inner(engine, &stmt, params))
    }
}

pub(in crate::sql) fn run_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            return super::view_triggers::run_view_delete_inner(engine, stmt, params);
        }
        if super::view_automatic::has_instead_of_trigger(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Delete,
        )? || crate::sql::rules::relation_suppresses_original_query(
            engine,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Delete,
        )? {
            return super::view_triggers::run_view_delete_inner(engine, stmt, params);
        }
        let rewritten = super::view_automatic::rewrite_delete_to_base(engine, stmt, params)?;
        return run_delete_inner(engine, &rewritten, params);
    }
    let _transition_capture_scope = crate::sql::triggers::TransitionCaptureScope::enter();
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    crate::sql::rules::validate_rule_returning_contract(
        engine,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Delete,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        crate::sql::rules::validate_rule_returning_contract(
            engine,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Delete,
            !view_returning.returning.is_empty(),
        )?;
    }
    let delete_rules = engine.rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Delete)?;
    let has_delete_rules = !delete_rules.is_empty();
    let has_view_delete_rules = !stmt.view_rule_relations.is_empty();
    let has_any_delete_rules = has_delete_rules || has_view_delete_rules;
    let view_original_query = !stmt.view_rule_relations.iter().try_fold(
        false,
        |suppressed, relation| -> Result<bool, SQLError> {
            Ok(suppressed
                || engine
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
        && !engine
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Delete,
                false,
                &[],
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if delete_original_query && !has_any_delete_rules {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut qualified_targets: Vec<(
        String,
        uqa_core::DocId,
        Document,
        Option<uqa_execution::OwnedPhysicalRow>,
    )> = Vec::new();
    let mut returning_rows = Vec::new();
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let mut action_qualification_count = if has_any_delete_rules && stmt.source.is_none() {
        super::row_independent_mutation_qualification_count(
            read_engine,
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
    let using_rows: Option<uqa_execution::SharedSpill> = match stmt.source.as_deref() {
        Some(source) => Some(build_join_spill_with_ctes(
            read_engine,
            source,
            params,
            &mut ctes,
        )?),
        None => None,
    };
    let qualification_references_target = if has_any_delete_rules && using_rows.is_some() {
        delete_qualification_references_target(read_engine, stmt, stmt.predicate.as_ref())?
    } else {
        false
    };
    if has_any_delete_rules {
        if let Some(using_rows) = using_rows.as_ref() {
            action_qualification_count = Some(if qualification_references_target {
                0
            } else {
                count_delete_source_qualifications(read_engine, stmt, &ctes, using_rows, params)?
            });
        }
    }
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        using_rows
            .as_ref()
            .map(uqa_execution::SharedSpill::row_schema),
    )?;
    let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
    // A non-volatile plain predicate can use the accelerated candidate set. A VOLATILE predicate must qualify rows in command order so each prior logical deletion is visible to the next callback.
    let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
        crate::sql::volatility::expr_contains_volatile_function(engine, predicate)
    });
    let preselected = !has_runtime_scope
        && stmt.source.is_none()
        && stmt.predicate.is_some()
        && !predicate_is_volatile;
    let target_tables = read_engine.hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
    let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("DELETE preselection is missing its predicate".into())
        })?;
        let mut candidates = Vec::new();
        for table in &target_tables {
            candidates.extend(
                crate::sql::where_eval::collect_where_doc_ids(
                    read_engine,
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
                read_engine
                    .table_doc_ids(table)?
                    .into_iter()
                    .map(|doc_id| (table.clone(), doc_id)),
            );
        }
        candidates
    };
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let qualification_overlay = DmlCommandMutationOverlay::new(engine);
    let mut qualified_ids = BTreeSet::new();
    for (storage_table, doc_id) in candidates {
        cancel.check()?;
        let candidate = if preselected {
            None
        } else {
            let candidate = qualified_delete_candidate(DeleteCandidateQualification {
                engine: read_engine,
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
        let target = lock_physical_mutation_target(
            engine,
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
            engine.refresh_explicit_statement_snapshot()?;
            if let Some((_, Some(source_context))) = candidate.as_ref() {
                recheck_delete_candidate(DeleteCandidateRecheck {
                    engine,
                    expression_engine: read_engine,
                    stmt,
                    storage_table: &storage_table,
                    params,
                    ctes: &snapshot_ctes,
                    doc_id,
                    source_context: Some(source_context),
                })?
            } else {
                recheck_delete_candidate(DeleteCandidateRecheck {
                    engine,
                    expression_engine: read_engine,
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
            engine
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
            qualified_targets.push((storage_table, doc_id, doc, returning_context));
        } else if crate::sql::triggers::fire_before_row_triggers(
            engine,
            &storage_table,
            uqa_sql::ast::TriggerEvent::Delete,
            doc_id,
            Some(&doc),
            None,
            &[],
        )?
        .is_some()
        {
            engine.stage_command_document(&storage_table, doc_id, None)?;
            qualified_targets.push((storage_table, doc_id, doc, returning_context));
        }
    }
    drop(qualification_overlay);
    let (view_rule_returning, rule_returning, to_delete) = if has_any_delete_rules {
        let rule_rows = qualified_targets
            .iter()
            .map(|(storage_table, doc_id, old, returning_context)| {
                crate::sql::rules::RuleRowImage {
                    old_storage_table: Some(storage_table.clone()),
                    old_doc_id: Some(*doc_id),
                    old: Some(old.clone()),
                    new_storage_table: None,
                    new_doc_id: None,
                    new: None,
                    context: returning_context.clone(),
                }
            })
            .collect::<Vec<_>>();
        let mut view_rule_batches =
            super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
                engine,
                relations: &stmt.view_rule_relations,
                event: uqa_sql::ast::RuleEvent::Delete,
                rows: &rule_rows,
                params,
                scope: &snapshot_ctes,
                insert_plans: &[],
                update_plans: &[],
                document_relation: None,
            })?;
        view_rule_batches.configure_action_qualification(action_qualification_count);
        let base_rule_indices = (0..rule_rows.len())
            .filter(|index| !view_rule_batches.suppresses(*index))
            .collect::<Vec<_>>();
        let mut rule_batch = (has_delete_rules && view_original_query)
            .then(|| {
                crate::sql::rules::prepare_rule_batch(
                    engine,
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
            let count = action_qualification_count.unwrap_or_else(|| rule_batch.event_row_count());
            rule_batch.set_action_qualification_count(count);
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
        if delete_original_query {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
            )?;
        }
        let qualification_overlay = DmlCommandMutationOverlay::new(engine);
        let mut to_delete = Vec::with_capacity(qualified_targets.len());
        for (index, (storage_table, doc_id, doc, returning_context)) in
            qualified_targets.into_iter().enumerate()
        {
            if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
                continue;
            }
            if crate::sql::triggers::fire_before_row_triggers(
                engine,
                &storage_table,
                uqa_sql::ast::TriggerEvent::Delete,
                doc_id,
                Some(&doc),
                None,
                &[],
            )?
            .is_none()
            {
                continue;
            }
            engine.stage_command_document(&storage_table, doc_id, None)?;
            to_delete.push((storage_table, doc_id, doc, returning_context));
        }
        drop(qualification_overlay);
        (view_rule_returning, rule_returning, to_delete)
    } else {
        (None, None, qualified_targets)
    };
    let root_deletes: BTreeSet<(String, DocId)> = to_delete
        .iter()
        .map(|(table, doc_id, _, _)| (table.clone(), *doc_id))
        .collect();
    let mut prepared_deletes = Vec::with_capacity(to_delete.len());
    let mut after_row_events = Vec::new();
    let mut referential_actions = super::ReferentialActionContext::default();
    let overlay = DmlCommandMutationOverlay::new(engine);
    for (storage_table, doc_id, _doc, returning_context) in to_delete {
        if let Some(mut prepared) = prepare_document_delete(
            engine,
            &storage_table,
            doc_id,
            params,
            &root_deletes,
            &mut referential_actions,
            false,
        )? {
            stage_prepared_document_delete(engine, &mut prepared, params, &mut after_row_events)?;
            affected += 1;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    ReturningProjectionRow {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        images: ReturningRowImages {
                            old: Some(ReturningRowImage {
                                storage_table: prepared.table.clone(),
                                doc_id: prepared.doc_id,
                                document: &prepared.document,
                            }),
                            new: None,
                        },
                        aliases: &stmt.returning_aliases,
                        context: returning_context.as_ref(),
                    },
                    &stmt.returning,
                    params,
                    &snapshot_ctes,
                )?);
            }
            prepared_deletes.push(prepared);
        }
    }
    drop(overlay);
    if !prepared_deletes.is_empty() {
        engine.prepare_explicit_transaction_writer()?;
        for prepared in &mut prepared_deletes {
            apply_validated_prepared_document_delete(engine, prepared)?;
        }
    }
    let transition_tables = if delete_original_query {
        crate::sql::triggers::build_transition_tables(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
            &after_row_events,
        )?
    } else {
        Vec::new()
    };
    let referential_transition =
        referential_actions.transition_tables(engine, &after_row_events)?;
    let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
    transition_refs.extend(referential_transition.iter());
    let root_events = delete_original_query
        .then_some(uqa_sql::ast::TriggerEvent::Delete)
        .into_iter()
        .collect::<Vec<_>>();
    for generation in crate::sql::triggers::after_trigger_generations(&transition_refs) {
        crate::sql::triggers::fire_after_row_trigger_events_for_generation(
            engine,
            &after_row_events,
            &transition_refs,
            generation,
        )?;
        referential_actions.fire_after_statement_triggers(
            engine,
            &referential_transition,
            &stmt.table,
            &root_events,
            generation,
        )?;
        if delete_original_query {
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
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
                engine,
                params,
                &ctes,
                using_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            );
        }
        let shape = DmlReturningShape {
            table: &stmt.table,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: using_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        };
        if let Some(rule_returning) = rule_returning {
            return rule_returning.project(engine, shape);
        }
        return dml_returning_result(engine, shape, returning_rows, affected);
    }
    Ok(SQLResult::from_affected(affected))
}

struct DeleteCandidateRecheck<'a> {
    engine: &'a Engine,
    expression_engine: &'a Engine,
    stmt: &'a DeletePlan,
    storage_table: &'a str,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    doc_id: DocId,
    source_context: Option<&'a uqa_execution::OwnedPhysicalRow>,
}

fn recheck_delete_candidate(
    context: DeleteCandidateRecheck<'_>,
) -> Result<Option<(Document, Option<uqa_execution::OwnedPhysicalRow>)>, SQLError> {
    let DeleteCandidateRecheck {
        engine,
        expression_engine,
        stmt,
        storage_table,
        params,
        ctes,
        doc_id,
        source_context,
    } = context;
    let Some(doc) = engine.get_document(storage_table, doc_id)? else {
        return Ok(None);
    };
    let target_row = dml_target_row_for_storage(
        engine,
        &stmt.table,
        storage_table,
        &stmt.target_qualifier,
        doc_id,
        &doc,
    )?;
    let joined = source_context
        .map(|source_context| dml_join_rows(&target_row, source_context))
        .unwrap_or(target_row);
    let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        eval_mutation_expr(expression_engine, ctes, filter, Some(&joined), params)
            .map(|value| uqa_sql::expr::truthy(&value))
    })?;
    Ok(qualifies.then(|| (doc, source_context.cloned())))
}

struct QualifiedDeleteCandidate {
    row: Option<(Document, Option<uqa_execution::OwnedPhysicalRow>)>,
    qualification_count: usize,
}

struct DeleteCandidateQualification<'a> {
    engine: &'a Engine,
    stmt: &'a DeletePlan,
    storage_table: &'a str,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    using_rows: Option<&'a uqa_execution::SharedSpill>,
    doc_id: DocId,
    count_all_qualifications: bool,
}

fn qualified_delete_candidate(
    context: DeleteCandidateQualification<'_>,
) -> Result<QualifiedDeleteCandidate, SQLError> {
    let DeleteCandidateQualification {
        engine,
        stmt,
        storage_table,
        params,
        ctes,
        using_rows,
        doc_id,
        count_all_qualifications,
    } = context;
    let Some(doc) = engine.get_document(storage_table, doc_id)? else {
        return Ok(QualifiedDeleteCandidate {
            row: None,
            qualification_count: 0,
        });
    };
    let target_row = dml_target_row_for_storage(
        engine,
        &stmt.table,
        storage_table,
        &stmt.target_qualifier,
        doc_id,
        &doc,
    )?;
    match using_rows {
        None => {
            let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
                eval_mutation_expr(engine, ctes, filter, Some(&target_row), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            Ok(QualifiedDeleteCandidate {
                row: qualifies.then_some((doc, None)),
                qualification_count: usize::from(qualifies),
            })
        }
        Some(rows) => {
            let reader = rows
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            let mut first = None;
            let mut qualification_count = 0;
            for using_row in reader {
                let source_context = using_row.map_err(crate::sql::select::physical_exec_error)?;
                let joined = dml_join_rows(&target_row, &source_context);
                let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
                    eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
                        .map(|value| uqa_sql::expr::truthy(&value))
                })?;
                if qualifies {
                    qualification_count += 1;
                    if first.is_none() {
                        first = Some(source_context);
                    }
                    if !count_all_qualifications {
                        break;
                    }
                }
            }
            Ok(QualifiedDeleteCandidate {
                row: first.map(|source_context| (doc, Some(source_context))),
                qualification_count,
            })
        }
    }
}

fn delete_qualification_references_target(
    engine: &Engine,
    stmt: &DeletePlan,
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
    let mut columns = BTreeSet::new();
    if !predicate.collect_columns(&mut columns) {
        return Ok(true);
    }
    let target_columns = engine
        .try_query_table_columns(&stmt.table)
        .map_err(|error| SQLError::Internal(format!("read DELETE target columns: {error}")))?
        .into_iter()
        .chain([
            super::DOC_ID_COLUMN.to_string(),
            super::TABLE_OID_COLUMN.to_string(),
            super::XMIN_COLUMN.to_string(),
        ])
        .collect::<BTreeSet<_>>();
    Ok(!columns.is_disjoint(&target_columns))
}

fn count_delete_source_qualifications(
    engine: &Engine,
    stmt: &DeletePlan,
    ctes: &CteScope,
    using_rows: &uqa_execution::SharedSpill,
    params: &[SQLParam],
) -> Result<usize, SQLError> {
    let mut count = 0;
    for source in using_rows
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

pub(in crate::sql) fn prepare_document_delete(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut super::ReferentialActionContext,
    fire_row_triggers: bool,
) -> Result<Option<PreparedDocumentDelete>, SQLError> {
    let key = (table.to_string(), doc_id);
    if referential_actions.delete_stack.contains(&key) {
        return Ok(None);
    }
    engine.lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    let target = lock_mutation_target(
        engine,
        table,
        table,
        doc_id,
        uqa_sql::ast::LockStrength::ForUpdate,
    )?;
    let MutationLockTarget::Present { doc_id, recheck } = target else {
        return Ok(None);
    };
    if recheck {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let identity = PhysicalDocumentIdentity {
        table: table.to_string(),
        doc_id,
    };
    let target = match referential_actions.pending_document(&identity) {
        Some(Some(document)) => document.clone(),
        Some(None) => return Ok(None),
        None => {
            let Some(document) = engine.get_document(table, doc_id)? else {
                return Ok(None);
            };
            document
        }
    };
    if fire_row_triggers
        && crate::sql::triggers::fire_before_row_triggers(
            engine,
            table,
            uqa_sql::ast::TriggerEvent::Delete,
            doc_id,
            Some(&target),
            None,
            &[],
        )?
        .is_none()
    {
        return Ok(None);
    }
    referential_actions
        .delete_stack
        .push((table.to_string(), doc_id));
    let actions = prepare_referenced_key_delete_actions(
        engine,
        ReferencedDelete {
            table,
            doc_id,
            document: &target,
        },
        params,
        root_deletes,
        referential_actions,
    );
    referential_actions.delete_stack.pop();
    let prepared = PreparedDocumentDelete {
        table: table.to_string(),
        doc_id,
        document: target,
        actions: actions?,
    };
    referential_actions.record_pending_document(identity, None);
    Ok(Some(prepared))
}

pub(in crate::sql) fn stage_prepared_document_delete(
    engine: &Engine,
    prepared: &mut PreparedDocumentDelete,
    params: &[SQLParam],
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
) -> Result<(), SQLError> {
    stage_prepared_document_delete_with_parent(engine, prepared, params, after_row_events, None)
}

pub(in crate::sql) fn stage_prepared_document_delete_with_parent(
    engine: &Engine,
    prepared: &mut PreparedDocumentDelete,
    params: &[SQLParam],
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    mut cascade_parent: Option<usize>,
) -> Result<(), SQLError> {
    engine.stage_command_document(&prepared.table, prepared.doc_id, None)?;
    if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
        engine,
        crate::sql::triggers::AfterRowTriggerInput {
            table: &prepared.table,
            event: uqa_sql::ast::TriggerEvent::Delete,
            old_doc_id: prepared.doc_id,
            new_doc_id: prepared.doc_id,
            old_document: Some(&prepared.document),
            new_document: None,
            updated_columns: &[],
            cascade_parent,
        },
    )? {
        cascade_parent = Some(crate::sql::triggers::AfterRowTriggerEvent::push(
            after_row_events,
            event,
        ));
    }
    for action in &mut prepared.actions {
        match action {
            PreparedDeleteAction::Delete(delete) => {
                stage_prepared_document_delete_with_parent(
                    engine,
                    delete,
                    params,
                    after_row_events,
                    cascade_parent,
                )?;
            }
            PreparedDeleteAction::Rewrite(rewrite) => {
                super::stage_prepared_document_rewrite_with_parent(
                    engine,
                    rewrite,
                    params,
                    None,
                    after_row_events,
                    cascade_parent,
                )?;
            }
        }
    }
    Ok(())
}

pub(in crate::sql) fn apply_validated_prepared_document_delete(
    engine: &Engine,
    prepared: &mut PreparedDocumentDelete,
) -> Result<(), SQLError> {
    for action in &mut prepared.actions {
        match action {
            PreparedDeleteAction::Delete(delete) => {
                apply_validated_prepared_document_delete(engine, delete)?;
            }
            PreparedDeleteAction::Rewrite(rewrite) => {
                apply_validated_prepared_document_rewrite(engine, rewrite)?;
            }
        }
    }
    engine.delete_document(&prepared.table, prepared.doc_id)
}

struct ReferencedDelete<'a> {
    table: &'a str,
    doc_id: DocId,
    document: &'a Document,
}

fn prepare_referenced_key_delete_actions(
    engine: &Engine,
    parent: ReferencedDelete<'_>,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut super::ReferentialActionContext,
) -> Result<Vec<PreparedDeleteAction>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(engine, parent.table)? {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| parent.document.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        engine.lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let comparison = foreign_key_comparison_types(engine, &ref_table, &fk)?;
        let expected = comparison.normalize(key_values.clone())?;
        let defer_no_action = matches!(fk.on_delete, ForeignKeyAction::NoAction)
            && engine.foreign_key_is_deferred(&ref_table, &fk)?;
        if defer_no_action {
            engine.defer_foreign_key_parent_event(&ref_table, parent.table, &fk)?;
        }
        if fk.period {
            let ordinary_len = expected.len().saturating_sub(1);
            let mut excluded_parents = root_deletes
                .iter()
                .map(|(table, doc_id)| super::PhysicalDocumentIdentity {
                    table: table.clone(),
                    doc_id: *doc_id,
                })
                .collect::<Vec<_>>();
            let parent_identity = super::PhysicalDocumentIdentity {
                table: parent.table.to_string(),
                doc_id: parent.doc_id,
            };
            if !excluded_parents.contains(&parent_identity) {
                excluded_parents.push(parent_identity);
            }
            for physical_table in engine.hierarchy_scan_tables(&ref_table, true)? {
                for child_id in engine.table_doc_ids(&physical_table)? {
                    if root_deletes.contains(&(physical_table.clone(), child_id)) {
                        continue;
                    }
                    let Some(child_doc) = engine.get_document(&physical_table, child_id)? else {
                        continue;
                    };
                    let Some(child_lookup) =
                        foreign_key_lookup_values(engine, &physical_table, &fk, &child_doc)?
                    else {
                        continue;
                    };
                    if child_lookup.values[..ordinary_len] != expected[..ordinary_len] {
                        continue;
                    }
                    if period_foreign_key_coverage(
                        engine,
                        &fk,
                        &child_lookup.values,
                        &excluded_parents,
                        None,
                    )?
                    .0
                    {
                        continue;
                    }
                    if defer_no_action {
                        engine.defer_foreign_key_check(
                            &ref_table,
                            parent.table,
                            &physical_table,
                            child_id,
                            &fk,
                        )?;
                        continue;
                    }
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
                            parent.table,
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
            }
            continue;
        }
        let statement_identity = format!(
            "{}:{}:{}",
            ref_table,
            fk.name.as_deref().unwrap_or("<unnamed>"),
            fk.local_columns.join(",")
        );
        match fk.on_delete {
            ForeignKeyAction::Cascade => referential_actions.trigger_statements.begin(
                engine,
                format!("{statement_identity}:on_delete_delete"),
                &ref_table,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
            )?,
            ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                let columns = delete_set_columns(&fk);
                referential_actions.trigger_statements.begin(
                    engine,
                    format!("{statement_identity}:on_delete_update"),
                    &ref_table,
                    uqa_sql::ast::TriggerEvent::Update,
                    &columns,
                )?;
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {}
        }
        let referencing = referencing_rows(
            engine,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            referential_actions,
        )?;
        for (child, _child_doc) in referencing {
            if root_deletes.contains(&(child.table.clone(), child.doc_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction if defer_no_action => {
                    engine.defer_foreign_key_check(
                        &ref_table,
                        parent.table,
                        &child.table,
                        child.doc_id,
                        &fk,
                    )?;
                }
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::Routine {
                        sqlstate: "23503".into(),
                        message: format!(
                            "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{ref_table}\"",
                            parent.table,
                            fk.name.as_deref().unwrap_or("<unnamed>")
                        ),
                    });
                }
                ForeignKeyAction::Cascade => {
                    if let Some(prepared) = prepare_document_delete(
                        engine,
                        &child.table,
                        child.doc_id,
                        params,
                        root_deletes,
                        referential_actions,
                        true,
                    )? {
                        actions.push(PreparedDeleteAction::Delete(Box::new(prepared)));
                    }
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let columns = delete_set_columns(&fk);
                    let Some((child, child_doc)) = engine.lock_referencing_child(
                        ReferencingChildLock {
                            ref_table: &ref_table,
                            child: &child,
                            lock_columns: &columns,
                            foreign_key: &fk,
                            comparison: &comparison,
                            expected: &expected,
                        },
                        referential_actions,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_doc.clone();
                    apply_set_action_to_child(
                        engine,
                        &child.table,
                        &child_doc,
                        &mut updated,
                        &columns,
                        fk.on_delete,
                        params,
                    )?;
                    if let Some(prepared) = prepare_referential_document_rewrite(
                        engine,
                        super::ReferentialRewritePreparation {
                            table: &child.table,
                            doc_id: child.doc_id,
                            old_document: child_doc,
                            proposed_document: updated,
                            updated_columns: columns,
                        },
                        params,
                        referential_actions,
                    )? {
                        actions.push(PreparedDeleteAction::Rewrite(Box::new(prepared)));
                    }
                }
            }
        }
    }
    Ok(actions)
}

pub(in crate::sql) fn delete_set_columns(fk: &ForeignKey) -> Vec<String> {
    if fk.on_delete_set_columns.is_empty() {
        fk.local_columns.clone()
    } else {
        fk.on_delete_set_columns.clone()
    }
}
