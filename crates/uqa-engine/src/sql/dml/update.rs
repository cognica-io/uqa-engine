//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE execution, point-update fast paths, and patch eligibility.

use super::{
    build_returning_row, coerce_to_column_type, dml_returning_result, dml_storage_error,
    dml_target_row_for_storage, eval_mutation_assignment, eval_mutation_expr,
    eval_view_rule_update_assignment, finish_mutation_publication, index_vectors_for_type,
    lock_mutation_target, lock_physical_mutation_target, prepare_partition_update_route,
    prepare_routed_document_rewrite, referrers_to_for_actions, run_update_from,
    stage_prepared_document_rewrite, update_lock_strength, validate_dml_expression_qualifiers,
    validate_mutation_columns, validate_returning_alias_relations, validate_view_checks, BTreeMap,
    BTreeSet, BinaryOp, ColumnType, CteScope, DmlReturningShape, Engine, MutationAssignmentTarget,
    MutationLockTarget, MutationOverlayScope, MutationPublicationBatch, MutationRewriteCandidate,
    MutationRowImage, MutationRowImages, PhysicalDocumentIdentity, PhysicalMutationLockTarget,
    PreparedMutationAction, ReturningProjectionRow, RowIndependentUpdateValues, SQLError, SQLParam,
    SQLResult, ScalarExpr, UpdatePlan, Value, ViewCheckContext,
};

pub(in crate::sql) fn run_update(
    engine: &Engine,
    stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    super::run_mutation_command(engine, move |engine| {
        run_update_inner(engine, &stmt, params)
    })
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(in crate::sql) fn run_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if let Some(kind) = super::view_triggers::target_view_kind(engine, &stmt.table)? {
        if kind == crate::StoredViewKind::Materialized {
            return super::view_triggers::run_view_update_inner(engine, stmt, params);
        }
        if super::view_automatic::has_instead_of_trigger(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Update,
        )? || crate::sql::rules::relation_suppresses_original_query(
            engine,
            &stmt.table,
            uqa_sql::ast::RuleEvent::Update,
        )? {
            return super::view_triggers::run_view_update_inner(engine, stmt, params);
        }
        let rewritten = super::view_automatic::rewrite_update_to_base(engine, stmt, params)?;
        return run_update_inner(engine, &rewritten, params);
    }
    let _transition_capture_scope = crate::sql::triggers::TransitionCaptureScope::enter();
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    crate::sql::rules::validate_rule_returning_contract(
        engine,
        &stmt.table,
        uqa_sql::ast::RuleEvent::Update,
        !stmt.returning.is_empty(),
    )?;
    if let Some(view_returning) = &stmt.view_rule_returning {
        crate::sql::rules::validate_rule_returning_contract(
            engine,
            &view_returning.relation,
            uqa_sql::ast::RuleEvent::Update,
            !view_returning.returning.is_empty(),
        )?;
    }
    if stmt.view_rule_update_plans.is_empty() {
        validate_mutation_columns(
            engine,
            &stmt.table,
            stmt.assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            "UPDATE",
        )?;
    }
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    let update_rules = engine.rules_for(&stmt.table, uqa_sql::ast::RuleEvent::Update)?;
    let has_update_rules = !update_rules.is_empty();
    let has_view_update_rules = !stmt.view_rule_relations.is_empty();
    let has_any_update_rules = has_update_rules || has_view_update_rules;
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
    let has_before_statement_trigger = update_original_query
        && !engine
            .triggers_for(
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                false,
                &assigned_columns,
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if update_original_query && !has_any_update_rules {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine(read_engine);
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);

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
        return run_update_from(engine, read_engine, stmt, source, params, &mut ctes);
    }
    let row_independent_update_qualification = if has_any_update_rules {
        super::row_independent_mutation_qualification_count(
            read_engine,
            stmt.predicate.as_ref(),
            params,
            &ctes,
        )?
    } else {
        None
    };
    let target_tables = read_engine.hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
    let target_hierarchy = read_engine
        .try_table_hierarchy(&stmt.table)
        .map_err(|error| SQLError::Internal(format!("read UPDATE hierarchy: {error}")))?;
    let target_is_partitioned =
        target_hierarchy.partition_spec.is_some() || target_hierarchy.partition_bound.is_some();
    let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
    if !has_runtime_scope
        && target_tables.len() == 1
        && !target_is_partitioned
        && statement_snapshot.is_none()
        && !engine.has_row_triggers(&stmt.table, uqa_sql::ast::TriggerEvent::Update)?
        && !crate::sql::triggers::transition_capture_required(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?
        && !engine.relation_has_rules(&stmt.table)?
        && !has_view_update_rules
    {
        if let Some(result) = try_run_point_update(engine, stmt, params)? {
            if update_original_query {
                crate::sql::triggers::fire_statement_triggers(
                    engine,
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
    let cancel = engine.cancellation_token();
    // A non-volatile predicate can still use the accelerated candidate set. A VOLATILE predicate must stay in the row loop because PostgreSQL exposes each preceding logical rewrite before qualifying the next candidate.
    let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
        crate::sql::volatility::expr_contains_volatile_function(engine, predicate)
    });
    let preselected = !has_runtime_scope && stmt.predicate.is_some() && !predicate_is_volatile;
    let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("UPDATE preselection is missing its predicate".into())
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
    let overlay = MutationOverlayScope::new(engine);
    let mut pending_updates = Vec::new();
    let mut prepared_updates = Vec::new();
    let mut events = super::MutationEventQueue::default();
    let mut locked_ids = BTreeSet::new();
    for (storage_table, doc_id) in candidates {
        cancel.check()?;
        let Some(candidate) = read_engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let candidate_row = dml_target_row_for_storage(
            read_engine,
            &stmt.table,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &candidate,
        )?;
        if !preselected {
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    read_engine,
                    &snapshot_ctes,
                    filter,
                    Some(&candidate_row),
                    params,
                )?) {
                    continue;
                }
            }
        }
        let target = lock_physical_mutation_target(
            engine,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            update_lock_strength(engine, &storage_table, &assigned_columns),
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
            engine.refresh_explicit_statement_snapshot()?;
        }
        let Some(mut doc) = engine.get_document_for_mutation(&storage_table, doc_id)? else {
            continue;
        };
        let original_doc = doc.clone();
        let target_row = dml_target_row_for_storage(
            engine,
            &stmt.table,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            &original_doc,
        )?;
        if recheck || preselected {
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    read_engine,
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
                    eval_mutation_assignment(
                        read_engine,
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
                    eval_view_rule_update_assignment(
                        read_engine,
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
        } else if let Some(prepared) = prepare_update_row(
            engine,
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
            .map(|candidate| crate::sql::rules::RuleRowImage {
                old_storage_table: Some(candidate.identity.table.clone()),
                old_doc_id: Some(candidate.identity.doc_id),
                old: Some(candidate.old_document.clone()),
                new_storage_table: Some(candidate.identity.table.clone()),
                new_doc_id: Some(candidate.identity.doc_id),
                new: Some(candidate.proposed_document.clone()),
                context: None,
            })
            .collect::<Vec<_>>();
        let mut view_rule_batches =
            super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
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
        view_rule_batches.configure_action_qualification(row_independent_update_qualification);
        let base_rule_indices = (0..rule_rows.len())
            .filter(|index| !view_rule_batches.suppresses(*index))
            .collect::<Vec<_>>();
        let mut rule_batch = (has_update_rules && view_original_query)
            .then(|| {
                crate::sql::rules::prepare_rule_batch(
                    engine,
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
        if update_original_query {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &stmt.table,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                &assigned_columns,
            )?;
        }
        let overlay = MutationOverlayScope::new(engine);
        for (index, candidate) in pending_updates.into_iter().enumerate() {
            if view_rule_batches.suppresses(index) || base_rule_suppressed[index] {
                continue;
            }
            if let Some(prepared) = prepare_update_row(
                engine,
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
            &stmt.table,
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
            &stmt.table,
            &root_events,
            generation,
        )?;
        if update_original_query {
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
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
            return view_rule_returning.project(engine, params, &ctes, None);
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
            return rule_returning.project(engine, shape);
        }
        return dml_returning_result(engine, shape, returning_rows, affected);
    }
    Ok(SQLResult::from_affected(affected))
}

struct PreparedUpdateRow {
    rewrite: super::PreparedDocumentRewrite,
    after_row_events: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    returning: Option<uqa_execution::OwnedPhysicalRow>,
    affected: bool,
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
fn prepare_update_row(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    snapshot_ctes: &CteScope,
    assigned_columns: &[String],
    storage_table: &str,
    doc_id: uqa_core::DocId,
    original_document: super::Document,
    document: super::Document,
    referential_actions: &mut super::ReferentialActionContext,
) -> Result<Option<PreparedUpdateRow>, SQLError> {
    let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
        engine,
        storage_table,
        uqa_sql::ast::TriggerEvent::Update,
        doc_id,
        Some(&original_document),
        Some(&document),
        assigned_columns,
    )?
    else {
        return Ok(None);
    };
    let Some(route) = prepare_partition_update_route(
        engine,
        storage_table,
        doc_id,
        &original_document,
        triggered_document,
        &stmt.table,
        params,
        stmt.include_descendants,
    )?
    else {
        return Ok(None);
    };
    let Some(mut rewrite) = prepare_routed_document_rewrite(
        engine,
        storage_table,
        doc_id,
        original_document,
        route,
        params,
        referential_actions,
    )?
    else {
        return Ok(None);
    };
    let primary_key_doc_id =
        super::integer_primary_key_doc_id(engine, &stmt.table, &rewrite.new_document)?;
    let rewritten_doc_id = rewrite
        .destination
        .as_ref()
        .map(|(_, doc_id)| *doc_id)
        .or(primary_key_doc_id)
        .unwrap_or(rewrite.doc_id);
    let rewritten_storage_table = rewrite
        .destination
        .as_ref()
        .map_or_else(|| rewrite.table.clone(), |(table, _)| table.clone());
    super::validate_key_constraints(
        engine,
        &rewritten_storage_table,
        &rewrite.new_document,
        (rewritten_storage_table == rewrite.table).then_some(rewrite.doc_id),
    )?;
    validate_view_checks(ViewCheckContext {
        engine,
        table: &stmt.table,
        storage_table: &rewritten_storage_table,
        target_qualifier: &stmt.target_qualifier,
        doc_id: rewritten_doc_id,
        document: &rewrite.new_document,
        checks: &stmt.view_checks,
        params,
        scope: snapshot_ctes,
    })?;
    let affected = !rewrite.is_partition_move_delete();
    let mut after_row_events = Vec::new();
    let rewritten_doc_id = stage_prepared_document_rewrite(
        engine,
        &mut rewrite,
        params,
        Some(assigned_columns),
        &mut after_row_events,
    )?;
    let returning = if !affected || stmt.returning.is_empty() {
        None
    } else {
        Some(build_returning_row(
            engine,
            ReturningProjectionRow {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                images: MutationRowImages {
                    old: Some(MutationRowImage {
                        storage_table: rewrite.table.clone(),
                        doc_id: rewrite.doc_id,
                        document: &rewrite.old_document,
                    }),
                    new: Some(MutationRowImage {
                        storage_table: rewritten_storage_table,
                        doc_id: rewritten_doc_id,
                        document: &rewrite.new_document,
                    }),
                },
                aliases: &stmt.returning_aliases,
                context: None,
            },
            &stmt.returning,
            params,
            snapshot_ctes,
        )?)
    };
    Ok(Some(PreparedUpdateRow {
        rewrite,
        after_row_events,
        returning,
        affected,
    }))
}

pub(in crate::sql) fn try_run_point_update(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if engine
        .try_describe_table(&stmt.table)
        .map_err(|error| dml_storage_error("UPDATE", error))?
        .is_some_and(|columns| columns.iter().any(|column| column.generated.is_some()))
    {
        return Ok(None);
    }
    if !stmt.returning.is_empty() {
        return Ok(None);
    }
    let Some((lookup_field, lookup_value)) =
        point_lookup_filter(stmt.predicate.as_ref(), engine, params)?
    else {
        return Ok(None);
    };
    let Some((updates, vectors)) = row_independent_update_values(engine, stmt, params)? else {
        return Ok(None);
    };
    if !can_patch_update_without_full_row(engine, &stmt.table, &updates)? {
        return Ok(None);
    }
    if matches!(lookup_value, Value::Null) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    if !point_lookup_field_is_unique(engine, &stmt.table, &lookup_field)? {
        return Ok(None);
    }
    let Some(doc_id) = engine.find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)?
    else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    let target = lock_mutation_target(
        engine,
        &stmt.table,
        &stmt.target_qualifier,
        doc_id,
        update_lock_strength(
            engine,
            &stmt.table,
            &stmt
                .assignments
                .iter()
                .map(|assignment| assignment.column.clone())
                .collect::<Vec<_>>(),
        ),
    )?;
    let MutationLockTarget::Present { doc_id, .. } = target else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    engine.prepare_explicit_transaction_writer()?;
    if engine.find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)? != Some(doc_id) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    let affected =
        engine.patch_document_fields_with_vector_values(&stmt.table, doc_id, &updates, &vectors)?;
    Ok(Some(SQLResult::from_affected(u64::from(affected))))
}

pub(in crate::sql) fn point_lookup_filter(
    filter: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Option<(String, Value)>, SQLError> {
    let Some(ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    }) = filter
    else {
        return Ok(None);
    };
    if let Some(field) = top_level_column(lhs) {
        if expr_is_row_independent(rhs) {
            let ctes = CteScope::new_for_current_routine(engine);
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, rhs, None, params)?,
            )));
        }
    }
    if let Some(field) = top_level_column(rhs) {
        if expr_is_row_independent(lhs) {
            let ctes = CteScope::new_for_current_routine(engine);
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, lhs, None, params)?,
            )));
        }
    }
    Ok(None)
}

pub(in crate::sql) fn top_level_column(expr: &ScalarExpr) -> Option<&str> {
    match expr {
        ScalarExpr::Column(name) => Some(name),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

pub(in crate::sql) fn row_independent_update_values(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<RowIndependentUpdateValues>, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctes = CteScope::new_for_current_routine(engine);
    for assignment in &stmt.assignments {
        if !expr_is_row_independent(&assignment.value) {
            return Ok(None);
        }
        let value = coerce_to_column_type(
            engine,
            &stmt.table,
            &assignment.column,
            eval_mutation_expr(engine, &ctes, &assignment.value, None, params)?,
        )?;
        if let Some(ty @ (ColumnType::Vector(_) | ColumnType::Tensor(_))) = engine
            .column_type(&stmt.table, &assignment.column)
            .map_err(|err| dml_storage_error("UPDATE", err))?
        {
            let values = index_vectors_for_type(&value, &ty)?;
            vectors.insert(assignment.column.clone(), values);
        }
        updates.insert(assignment.column.clone(), value);
    }
    Ok(Some((updates, vectors)))
}

pub(in crate::sql) fn expr_is_row_independent(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(expr_is_row_independent),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_is_row_independent(lhs) && expr_is_row_independent(rhs)
        }
        ScalarExpr::Not(inner) | ScalarExpr::UnaryMinus(inner) => expr_is_row_independent(inner),
        ScalarExpr::IsNull { expr, .. } => expr_is_row_independent(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_is_row_independent(expr)
                && expr_is_row_independent(low)
                && expr_is_row_independent(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_is_row_independent(expr) && list.iter().all(expr_is_row_independent)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().map_or(true, expr_is_row_independent)
                && when.iter().all(|(condition, result)| {
                    expr_is_row_independent(condition) && expr_is_row_independent(result)
                })
                && else_branch.as_deref().map_or(true, expr_is_row_independent)
        }
        ScalarExpr::Cast { expr, .. } => expr_is_row_independent(expr),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Func { .. }
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

pub(in crate::sql) fn can_patch_update_without_full_row(
    engine: &Engine,
    table: &str,
    updates: &BTreeMap<String, Value>,
) -> Result<bool, SQLError> {
    if engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.enforced)
    {
        return Ok(false);
    }
    let update_keys: BTreeSet<&str> = updates.keys().map(String::as_str).collect();
    if engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .iter()
        .any(|col| {
            col.not_null
                && col.auto_increment.is_none()
                && matches!(updates.get(&col.name), Some(Value::Null))
        })
    {
        return Ok(false);
    }
    if engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| {
            constraint
                .columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .filter(|fk| fk.enforced)
        .any(|fk| {
            fk.local_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if referrers_to_for_actions(engine, table)?
        .iter()
        .any(|(_, fk)| {
            fk.ref_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    Ok(true)
}

pub(in crate::sql) fn point_lookup_field_is_unique(
    engine: &Engine,
    table: &str,
    lookup_field: &str,
) -> Result<bool, SQLError> {
    Ok(engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.columns.len() == 1 && constraint.columns[0] == lookup_field))
}
