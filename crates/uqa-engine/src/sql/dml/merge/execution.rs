//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build_join_spill_with_ctes, build_merge_returning_row, decode_merge_pair,
    decode_prepared_mutation_action_row, dml_join_rows, dml_null_target_row,
    dml_returning_result_with_projections, dml_storage_error, dml_target_row_for_storage,
    encode_merge_pair, ensure_merge_target_is_modified_once, eval_mutation_expr,
    expanded_merge_returning_projections, finish_mutation_publication, insert_identity_columns,
    lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
    lock_physical_mutation_target, merge_pair_schema, merge_returning_source_schema,
    merge_source_index_value, merge_target_lock_strength, missing_document_error,
    partition_insert_target, persist_auto_increment_identity, prepare_auto_increment_identity,
    prepare_document_delete, prepare_insert_identity, prepare_partition_update_route,
    prepare_routed_document_rewrite, prepared_mutation_action_schema,
    push_prepared_mutation_action, refresh_insert_identity_after_trigger, select_merge_action,
    stage_prepared_document_delete, stage_prepared_document_rewrite, validate_document_constraints,
    validate_merge_action_scopes, validate_mutation_columns, validate_returning_alias_relations,
    validate_view_checks, validate_view_merge_dispatch_contract, BTreeMap, BTreeSet, CteScope,
    DmlReturningShape, Document, Engine, MergePairKind, MergePlan, MergeReturningRow,
    MergeWhenPlan, MutationOverlayScope, MutationPublicationBatch, MutationRowImage,
    MutationRowImages, PhysicalMutationLockTarget, PreparedDocumentInsert, PreparedMutationAction,
    SQLError, SQLParam, SQLResult, ViewCheckContext,
};

mod model;
mod privileges;

pub(super) use model::{MergeTargetIdentity, SelectedMergeAction};

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(super) fn run_merge_inner(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::truthy;
    if super::super::view_triggers::target_view_kind(engine, &stmt.target)?.is_some() {
        validate_view_merge_dispatch_contract(engine, stmt, params)?;
        return match super::super::view_automatic::merge_view_target_path(engine, stmt)? {
            super::super::view_automatic::MergeViewTargetPath::AutomaticRewrite => {
                let rewritten =
                    super::super::view_automatic::rewrite_merge_to_base(engine, stmt, params)?;
                run_merge_inner(engine, &rewritten, params)
            }
            super::super::view_automatic::MergeViewTargetPath::ViewTriggers => {
                let _ = super::super::view_privileges::ensure_merge(engine, stmt)?;
                super::super::view_triggers::run_view_merge_inner(engine, stmt, params)
            }
        };
    }
    privileges::ensure_merge_privileges(engine, stmt)?;
    let _transition_capture_scope = crate::sql::triggers::TransitionCaptureScope::enter();
    let target_table = stmt.target.clone();
    engine.lock_relation(
        &target_table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    if [
        uqa_sql::ast::RuleEvent::Insert,
        uqa_sql::ast::RuleEvent::Update,
        uqa_sql::ast::RuleEvent::Delete,
    ]
    .into_iter()
    .map(|event| engine.rules_for(&target_table, event))
    .collect::<Result<Vec<_>, SQLError>>()?
    .iter()
    .any(|rules| !rules.is_empty())
    {
        let relation =
            crate::RelationIdentity::from_legacy_name(&target_table).map_err(|error| {
                SQLError::Internal(format!("decode MERGE relation `{target_table}`: {error}"))
            })?;
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot execute MERGE on relation \"{}\"", relation.name),
        });
    }
    let target_qual = stmt.target_qualifier.clone();
    let target_tables = engine.hierarchy_scan_tables(&target_table, stmt.include_descendants)?;
    let mut ctes = CteScope::new_for_statement(engine, stmt.statement_privilege_subject.as_deref());
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validate_mutation_columns(
                    engine,
                    &target_table,
                    assignments
                        .iter()
                        .map(|assignment| assignment.column.as_str()),
                    "MERGE UPDATE",
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } => validate_mutation_columns(
                engine,
                &target_table,
                columns.iter().map(String::as_str),
                "MERGE INSERT",
            )?,
            _ => {}
        }
    }
    let has_insert_action = stmt
        .when_clauses
        .iter()
        .any(|clause| matches!(clause, MergeWhenPlan::InsertNotMatched { .. }));
    let has_update_action = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. }
        )
    });
    let has_delete_action = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        )
    });
    let update_statement_columns = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => Some(assignments),
            _ => None,
        })
        .flatten()
        .map(|assignment| assignment.column.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let source_rows = build_join_spill_with_ctes(engine, &stmt.source, params, &mut ctes)?;
    let returning_source_relation = uqa_sql::ast::InternalRelationId::allocate();
    validate_returning_alias_relations(
        &target_qual,
        &stmt.returning_aliases,
        Some(source_rows.row_schema()),
    )?;
    let null_target_row = dml_null_target_row(engine, &target_table, &target_qual)?;
    validate_merge_action_scopes(
        engine,
        stmt,
        &null_target_row.schema,
        source_rows.row_schema(),
        params,
    )?;
    for (enabled, event, columns) in [
        (
            has_insert_action,
            uqa_sql::ast::TriggerEvent::Insert,
            &[][..],
        ),
        (
            has_update_action,
            uqa_sql::ast::TriggerEvent::Update,
            update_statement_columns.as_slice(),
        ),
        (
            has_delete_action,
            uqa_sql::ast::TriggerEvent::Delete,
            &[][..],
        ),
    ] {
        if enabled {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &target_table,
                uqa_sql::ast::TriggerTiming::Before,
                event,
                columns,
            )?;
        }
    }
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();

    let pair_schema = merge_pair_schema(source_rows.row_schema());
    let work_mem = crate::sql::select::physical_work_mem_bytes(engine.query_runtime_view())?.max(1);
    let mut pairings = uqa_execution::SpillBuffer::new(work_mem);
    let mut matched_source = uqa_execution::ExactRowSet::new(work_mem);
    let mut lock_target_ids = BTreeSet::new();
    let matched_can_mutate = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::DeleteMatched { .. }
        )
    });
    let source_missing_can_mutate = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        )
    });
    let has_source_missing_clause = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        )
    });
    let null_source_row = uqa_execution::OwnedPhysicalRow::new(
        source_rows.row_schema().clone(),
        uqa_execution::PhysicalRow::nulls(source_rows.row_schema().physical_width()),
    );

    for storage_table in &target_tables {
        for doc_id in &engine.table_doc_ids(storage_table)? {
            let Some(doc) = engine.get_document(storage_table, *doc_id)? else {
                return Err(missing_document_error("MERGE scan", storage_table, *doc_id));
            };
            let target_row = dml_target_row_for_storage(
                engine,
                &target_table,
                storage_table,
                &target_qual,
                *doc_id,
                &doc,
            )?;
            if let Some(predicate) = &stmt.target_predicate {
                if !truthy(&eval_mutation_expr(
                    engine,
                    &ctes,
                    predicate,
                    Some(&target_row),
                    params,
                )?) {
                    continue;
                }
            }
            let mut target_matched = false;
            let source_reader = source_rows
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            for (idx, src) in source_reader.enumerate() {
                let src = src.map_err(crate::sql::select::physical_exec_error)?;
                let joined = dml_join_rows(&target_row, &src);
                if truthy(&eval_mutation_expr(
                    engine,
                    &ctes,
                    &stmt.join_condition,
                    Some(&joined),
                    params,
                )?) {
                    target_matched = true;
                    let index_value = merge_source_index_value(idx);
                    let _ = matched_source
                        .insert_values(std::slice::from_ref(&index_value))
                        .map_err(crate::sql::select::physical_exec_error)?;
                    pairings
                        .push(uqa_execution::Batch::from_physical_rows(
                            pair_schema.clone(),
                            vec![encode_merge_pair(
                                MergePairKind::Matched,
                                Some(storage_table),
                                Some(*doc_id),
                                Some(&doc),
                                &src,
                            )],
                        ))
                        .map_err(crate::sql::select::physical_exec_error)?;
                }
            }
            if target_matched && matched_can_mutate {
                lock_target_ids.insert((storage_table.clone(), *doc_id));
            } else if !target_matched && has_source_missing_clause {
                if source_missing_can_mutate {
                    lock_target_ids.insert((storage_table.clone(), *doc_id));
                }
                pairings
                    .push(uqa_execution::Batch::from_physical_rows(
                        pair_schema.clone(),
                        vec![encode_merge_pair(
                            MergePairKind::NotMatchedBySource,
                            Some(storage_table),
                            Some(*doc_id),
                            Some(&doc),
                            &null_source_row,
                        )],
                    ))
                    .map_err(crate::sql::select::physical_exec_error)?;
            }
        }
    }
    let source_reader = source_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for (idx, src) in source_reader.enumerate() {
        let src = src.map_err(crate::sql::select::physical_exec_error)?;
        let index_value = merge_source_index_value(idx);
        if matched_source
            .contains_values(std::slice::from_ref(&index_value))
            .map_err(crate::sql::select::physical_exec_error)?
        {
            continue;
        }
        pairings
            .push(uqa_execution::Batch::from_physical_rows(
                pair_schema.clone(),
                vec![encode_merge_pair(
                    MergePairKind::NotMatchedByTarget,
                    None,
                    None,
                    None,
                    &src,
                )],
            ))
            .map_err(crate::sql::select::physical_exec_error)?;
    }

    let pairings = pairings
        .into_shared(pair_schema)
        .map_err(crate::sql::select::physical_exec_error)?;
    let mut recheck_matches = false;
    // A paired target may have been moved to a successor identity by a primary-key rewrite another transaction committed while this statement waited; PostgreSQL 18 follows the update chain, so the pairing is redirected to the successor before the actions run.
    let mut successors: BTreeMap<MergeTargetIdentity, MergeTargetIdentity> = BTreeMap::new();
    let mut rechecked_target_ids = BTreeSet::new();
    let mut deleted_targets = BTreeSet::new();
    for (storage_table, doc_id) in lock_target_ids {
        let original_identity = (storage_table.clone(), doc_id);
        let target = lock_physical_mutation_target(
            engine,
            &storage_table,
            &target_qual,
            doc_id,
            merge_target_lock_strength(engine, stmt, &storage_table),
        )?;
        match target {
            PhysicalMutationLockTarget::Present { identity, recheck } => {
                recheck_matches |= recheck;
                let locked_identity = (identity.table, identity.doc_id);
                if recheck || locked_identity != original_identity {
                    rechecked_target_ids.insert(original_identity.clone());
                }
                if locked_identity != original_identity {
                    successors.insert(original_identity, locked_identity);
                }
            }
            PhysicalMutationLockTarget::Deleted => {
                recheck_matches = true;
                deleted_targets.insert(original_identity);
            }
        }
    }
    if recheck_matches {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let mut refreshed_targets = BTreeMap::new();
    for original_identity in rechecked_target_ids {
        let locked_identity = successors
            .get(&original_identity)
            .cloned()
            .unwrap_or_else(|| original_identity.clone());
        if let Some(document) = engine.get_document(&locked_identity.0, locked_identity.1)? {
            refreshed_targets.insert(original_identity, (locked_identity, document));
        } else {
            deleted_targets.insert(original_identity);
        }
    }
    let action_schema = prepared_mutation_action_schema();
    let mut prepared_actions = uqa_execution::SpillBuffer::new(work_mem);
    let mut events = super::super::MutationEventQueue::default();
    let mut root_deletes = BTreeSet::new();
    let mut has_mutation = false;
    let mut mutated_target_ids = BTreeSet::new();
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let overlay = MutationOverlayScope::new(engine);
    let pairing_reader = pairings
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for pair in pairing_reader {
        let pair = pair.map_err(crate::sql::select::physical_exec_error)?;
        let mut pair = decode_merge_pair(pair)?;
        let original_identity = pair
            .storage_table
            .as_ref()
            .zip(pair.doc_id)
            .map(|(table, doc_id)| (table.clone(), doc_id));
        if original_identity
            .as_ref()
            .is_some_and(|identity| deleted_targets.contains(identity))
        {
            match pair.kind {
                MergePairKind::Matched => {
                    pair.kind = MergePairKind::NotMatchedByTarget;
                    pair.storage_table = None;
                    pair.doc_id = None;
                    pair.target_document = None;
                }
                MergePairKind::NotMatchedBySource => continue,
                MergePairKind::NotMatchedByTarget => {}
            }
        } else if let Some(((successor_table, successor_doc_id), document)) = original_identity
            .as_ref()
            .and_then(|identity| refreshed_targets.get(identity))
        {
            pair.storage_table = Some(successor_table.clone());
            pair.doc_id = Some(*successor_doc_id);
            pair.target_document = Some(document.clone());
        }
        let mut target_row = match (pair.doc_id, pair.target_document.as_ref()) {
            (Some(doc_id), Some(document)) => dml_target_row_for_storage(
                engine,
                &target_table,
                pair.storage_table.as_deref().ok_or_else(|| {
                    SQLError::Internal("MERGE target row lost its physical relation".into())
                })?,
                &target_qual,
                doc_id,
                document,
            )?,
            _ => dml_null_target_row(engine, &target_table, &target_qual)?,
        };
        let mut joined = dml_join_rows(&target_row, &pair.source_row);
        if recheck_matches && pair.doc_id.is_some() {
            let target_visible = match &stmt.target_predicate {
                Some(predicate) => truthy(&eval_mutation_expr(
                    engine,
                    &snapshot_ctes,
                    predicate,
                    Some(&target_row),
                    params,
                )?),
                None => true,
            };
            if !target_visible {
                match pair.kind {
                    MergePairKind::Matched => {
                        pair.kind = MergePairKind::NotMatchedByTarget;
                        pair.storage_table = None;
                        pair.doc_id = None;
                        pair.target_document = None;
                        target_row = dml_null_target_row(engine, &target_table, &target_qual)?;
                        joined = dml_join_rows(&target_row, &pair.source_row);
                    }
                    MergePairKind::NotMatchedBySource => continue,
                    MergePairKind::NotMatchedByTarget => {}
                }
            }
        }
        if matches!(pair.kind, MergePairKind::Matched)
            && recheck_matches
            && !uqa_sql::expr::truthy(&eval_mutation_expr(
                engine,
                &snapshot_ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?)
        {
            pair.kind = MergePairKind::NotMatchedByTarget;
            pair.storage_table = None;
            pair.doc_id = None;
            pair.target_document = None;
            target_row = dml_null_target_row(engine, &target_table, &target_qual)?;
            joined = dml_join_rows(&target_row, &pair.source_row);
        }
        let action_row = match pair.kind {
            MergePairKind::Matched => &joined,
            MergePairKind::NotMatchedBySource => &target_row,
            MergePairKind::NotMatchedByTarget => &pair.source_row,
        };
        match select_merge_action(
            engine,
            stmt,
            &target_table,
            pair.kind,
            pair.doc_id,
            pair.target_document.as_ref(),
            action_row,
            params,
            &snapshot_ctes,
        )? {
            SelectedMergeAction::Nothing => {}
            SelectedMergeAction::Update {
                doc_id,
                old_document,
                new_document,
                updated_columns,
            } => {
                let storage_table = pair.storage_table.as_deref().ok_or_else(|| {
                    SQLError::Internal("MERGE update lost its physical target table".into())
                })?;
                let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
                    engine,
                    storage_table,
                    uqa_sql::ast::TriggerEvent::Update,
                    doc_id,
                    Some(&old_document),
                    Some(&new_document),
                    &updated_columns,
                )?
                else {
                    continue;
                };
                ensure_merge_target_is_modified_once(
                    &mut mutated_target_ids,
                    storage_table,
                    doc_id,
                )?;
                let Some(route) = prepare_partition_update_route(
                    engine,
                    storage_table,
                    doc_id,
                    &old_document,
                    triggered_document,
                    &target_table,
                    params,
                    true,
                )?
                else {
                    continue;
                };
                let mut prepared = prepare_routed_document_rewrite(
                    engine,
                    storage_table,
                    doc_id,
                    old_document,
                    route,
                    params,
                    events.referential_actions_mut(),
                )?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "MERGE rewrite dependency tree was cyclic at its root".into(),
                    )
                })?;
                prepared.capture_partition_move_update_transition = false;
                let row_affected = !prepared.is_partition_move_delete();
                let old_storage_table = prepared.table.clone();
                let new_storage_table = prepared
                    .destination
                    .as_ref()
                    .map_or_else(|| old_storage_table.clone(), |(table, _)| table.clone());
                let primary_key_doc_id = super::super::integer_primary_key_doc_id(
                    engine,
                    &target_table,
                    &prepared.new_document,
                )?;
                let checked_doc_id = prepared
                    .destination
                    .as_ref()
                    .map(|(_, doc_id)| *doc_id)
                    .or(primary_key_doc_id)
                    .unwrap_or(prepared.doc_id);
                validate_view_checks(ViewCheckContext {
                    engine,
                    table: &target_table,
                    storage_table: &new_storage_table,
                    target_qualifier: &target_qual,
                    doc_id: checked_doc_id,
                    document: &prepared.new_document,
                    checks: &stmt.view_checks,
                    params,
                    scope: &snapshot_ctes,
                })?;
                let old_metadata = super::super::existing_tuple_metadata(
                    engine,
                    &prepared.table,
                    prepared.doc_id,
                )?;
                let new_metadata = super::super::new_tuple_metadata(engine)?;
                let rewritten_doc_id = stage_prepared_document_rewrite(
                    engine,
                    &mut prepared,
                    params,
                    Some(&updated_columns),
                    events.after_rows_mut(),
                )?;
                if row_affected && !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: MutationRowImages {
                                old: Some(MutationRowImage {
                                    storage_table: old_storage_table,
                                    doc_id: prepared.doc_id,
                                    document: &prepared.old_document,
                                    metadata: old_metadata,
                                }),
                                new: Some(MutationRowImage {
                                    storage_table: new_storage_table,
                                    doc_id: rewritten_doc_id,
                                    document: &prepared.new_document,
                                    metadata: new_metadata,
                                }),
                            },
                            returning_aliases: &stmt.returning_aliases,
                            source_row: &pair.source_row,
                            source_schema: source_rows.row_schema(),
                            source_relation: returning_source_relation,
                            action: "UPDATE",
                        },
                        &stmt.returning,
                        params,
                        &snapshot_ctes,
                    )?);
                }
                affected += u64::from(row_affected);
                has_mutation = true;
                push_prepared_mutation_action(
                    &mut prepared_actions,
                    &action_schema,
                    PreparedMutationAction::Rewrite(prepared),
                )?;
            }
            SelectedMergeAction::Delete { doc_id } => {
                let storage_table = pair.storage_table.as_deref().ok_or_else(|| {
                    SQLError::Internal("MERGE delete lost its physical target table".into())
                })?;
                let old_document = pair
                    .target_document
                    .as_ref()
                    .ok_or_else(|| SQLError::Internal("MERGE delete lost its target row".into()))?;
                if crate::sql::triggers::fire_before_row_triggers(
                    engine,
                    storage_table,
                    uqa_sql::ast::TriggerEvent::Delete,
                    doc_id,
                    Some(old_document),
                    None,
                    &[],
                )?
                .is_none()
                {
                    continue;
                }
                ensure_merge_target_is_modified_once(
                    &mut mutated_target_ids,
                    storage_table,
                    doc_id,
                )?;
                root_deletes.insert((storage_table.to_string(), doc_id));
                let mut prepared = prepare_document_delete(
                    engine,
                    storage_table,
                    doc_id,
                    params,
                    &root_deletes,
                    events.referential_actions_mut(),
                    false,
                )?
                .ok_or_else(|| {
                    SQLError::Internal("MERGE delete dependency tree was cyclic at its root".into())
                })?;
                let old_metadata = super::super::existing_tuple_metadata(
                    engine,
                    &prepared.table,
                    prepared.doc_id,
                )?;
                stage_prepared_document_delete(
                    engine,
                    &mut prepared,
                    params,
                    events.after_rows_mut(),
                )?;
                if !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: MutationRowImages {
                                old: Some(MutationRowImage {
                                    storage_table: prepared.table.clone(),
                                    doc_id: prepared.doc_id,
                                    document: &prepared.document,
                                    metadata: old_metadata,
                                }),
                                new: None,
                            },
                            returning_aliases: &stmt.returning_aliases,
                            source_row: &pair.source_row,
                            source_schema: source_rows.row_schema(),
                            source_relation: returning_source_relation,
                            action: "DELETE",
                        },
                        &stmt.returning,
                        params,
                        &snapshot_ctes,
                    )?);
                }
                affected += 1;
                has_mutation = true;
                push_prepared_mutation_action(
                    &mut prepared_actions,
                    &action_schema,
                    PreparedMutationAction::Delete(prepared),
                )?;
            }
            SelectedMergeAction::Insert { mut document } => {
                let (auto_id_col, id_column, accepts_supplied_identity) =
                    insert_identity_columns(engine, &target_table, "MERGE INSERT")?;
                let prepared_auto_identity = prepare_auto_increment_identity(
                    engine,
                    &target_table,
                    &id_column,
                    auto_id_col.as_deref(),
                    &mut document,
                    "prepare MERGE INSERT identity",
                )?;
                // MERGE INTO ONLY excludes descendants from matching, while PostgreSQL still routes INSERT actions through the target's partition tree.
                let storage_table =
                    partition_insert_target(engine, &target_table, &document, params, true)?;
                engine.lock_relation(
                    &storage_table,
                    crate::row_locks::RelationLockMode::RowExclusive,
                )?;
                let mut insert_identity = match prepared_auto_identity {
                    Some(identity) => identity,
                    None => prepare_insert_identity(
                        engine,
                        &storage_table,
                        &id_column,
                        accepts_supplied_identity,
                        None,
                        &mut document,
                        "prepare MERGE INSERT identity",
                    )?,
                };
                let doc_id = insert_identity.0;
                let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
                    engine,
                    &storage_table,
                    uqa_sql::ast::TriggerEvent::Insert,
                    doc_id,
                    None,
                    Some(&document),
                    &[],
                )?
                else {
                    continue;
                };
                document = triggered_document;
                crate::sql::generated::refresh_stored_generated_columns(
                    engine,
                    &storage_table,
                    &mut document,
                )?;
                refresh_insert_identity_after_trigger(
                    engine,
                    &storage_table,
                    &id_column,
                    accepts_supplied_identity,
                    auto_id_col.as_deref(),
                    &document,
                    &mut insert_identity,
                )?;
                let doc_id = insert_identity.0;
                let trigger_target =
                    partition_insert_target(engine, &target_table, &document, params, true)?;
                if trigger_target != storage_table {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "moving row to another partition during a BEFORE FOR EACH ROW trigger is not supported".into(),
                    });
                }
                lock_existing_document_foreign_key_dependencies(engine, &storage_table, &document)?;
                let _key_locks =
                    lock_document_key_dependencies(engine, &storage_table, &document, None)?;
                validate_document_constraints(engine, &storage_table, &document, params, None)?;
                validate_view_checks(ViewCheckContext {
                    engine,
                    table: &target_table,
                    storage_table: &storage_table,
                    target_qualifier: &target_qual,
                    doc_id,
                    document: &document,
                    checks: &stmt.view_checks,
                    params,
                    scope: &snapshot_ctes,
                })?;
                engine.stage_command_document(&storage_table, doc_id, Some(document.clone()))?;
                if let Some(event) = crate::sql::triggers::AfterRowTriggerEvent::prepare(
                    engine,
                    crate::sql::triggers::AfterRowTriggerInput {
                        table: &storage_table,
                        event: uqa_sql::ast::TriggerEvent::Insert,
                        old_doc_id: doc_id,
                        new_doc_id: doc_id,
                        old_document: None,
                        new_document: Some(&document),
                        updated_columns: &[],
                        cascade_parent: None,
                    },
                )? {
                    crate::sql::triggers::AfterRowTriggerEvent::push(
                        events.after_rows_mut(),
                        event,
                    );
                }
                if !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: MutationRowImages {
                                old: None,
                                new: Some(MutationRowImage {
                                    storage_table: storage_table.clone(),
                                    doc_id,
                                    document: &document,
                                    metadata: super::super::new_tuple_metadata(engine)?,
                                }),
                            },
                            returning_aliases: &stmt.returning_aliases,
                            source_row: &pair.source_row,
                            source_schema: source_rows.row_schema(),
                            source_relation: returning_source_relation,
                            action: "INSERT",
                        },
                        &stmt.returning,
                        params,
                        &snapshot_ctes,
                    )?);
                }
                affected += 1;
                has_mutation = true;
                push_prepared_mutation_action(
                    &mut prepared_actions,
                    &action_schema,
                    PreparedMutationAction::Insert(PreparedDocumentInsert {
                        table: storage_table,
                        doc_id,
                        document,
                    }),
                )?;
            }
        }
    }
    drop(overlay);
    let prepared_actions = prepared_actions
        .into_shared(action_schema)
        .map_err(crate::sql::select::physical_exec_error)?;
    if has_mutation {
        engine.prepare_explicit_transaction_writer()?;
        let auto_id_column = engine
            .auto_increment_column(&target_table)
            .map_err(|error| dml_storage_error("MERGE INSERT", error))?;
        persist_auto_increment_identity(
            engine,
            &target_table,
            auto_id_column.as_deref(),
            "persist MERGE INSERT identity",
        )?;
    }
    let prepared_reader = prepared_actions
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    let mut publication = MutationPublicationBatch::default();
    for prepared in prepared_reader {
        let prepared = prepared.map_err(crate::sql::select::physical_exec_error)?;
        let action = decode_prepared_mutation_action_row(prepared)?;
        super::super::publish_prepared_mutation_action(engine, action, false, &mut publication)?;
    }
    finish_mutation_publication(engine, &mut publication)?;
    let delete_transition = if has_delete_action {
        crate::sql::triggers::build_transition_tables(
            engine,
            &target_table,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let update_transition = if has_update_action {
        crate::sql::triggers::build_transition_tables(
            engine,
            &target_table,
            uqa_sql::ast::TriggerEvent::Update,
            &update_statement_columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let insert_transition = if has_insert_action {
        crate::sql::triggers::build_transition_tables(
            engine,
            &target_table,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(engine)?;
    let mut transition_tables = delete_transition
        .iter()
        .chain(update_transition.iter())
        .chain(insert_transition.iter())
        .collect::<Vec<_>>();
    transition_tables.extend(referential_transition.iter());
    let root_events = [
        (has_delete_action, uqa_sql::ast::TriggerEvent::Delete),
        (has_update_action, uqa_sql::ast::TriggerEvent::Update),
        (has_insert_action, uqa_sql::ast::TriggerEvent::Insert),
    ]
    .into_iter()
    .filter_map(|(enabled, event)| enabled.then_some(event))
    .collect::<Vec<_>>();
    for generation in crate::sql::triggers::after_trigger_generations(&transition_tables) {
        crate::sql::triggers::fire_after_row_trigger_events_for_generation(
            engine,
            events.after_rows(),
            &transition_tables,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            engine,
            &referential_transition,
            &target_table,
            &root_events,
            generation,
        )?;
        for (enabled, event, columns) in [
            (
                has_delete_action,
                uqa_sql::ast::TriggerEvent::Delete,
                &[][..],
            ),
            (
                has_update_action,
                uqa_sql::ast::TriggerEvent::Update,
                update_statement_columns.as_slice(),
            ),
            (
                has_insert_action,
                uqa_sql::ast::TriggerEvent::Insert,
                &[][..],
            ),
        ] {
            if !enabled {
                continue;
            }
            let event_transitions = match event {
                uqa_sql::ast::TriggerEvent::Delete => &delete_transition,
                uqa_sql::ast::TriggerEvent::Update => &update_transition,
                uqa_sql::ast::TriggerEvent::Insert => &insert_transition,
                uqa_sql::ast::TriggerEvent::Truncate => unreachable!(),
            };
            crate::sql::triggers::fire_after_statement_trigger_generation_for_root(
                engine,
                &target_table,
                event,
                columns,
                event_transitions,
                generation,
            )?;
        }
    }
    if !stmt.returning.is_empty() {
        let projections = expanded_merge_returning_projections(
            engine,
            &target_table,
            &target_qual,
            &stmt.returning_aliases,
            source_rows.row_schema(),
            returning_source_relation,
            &stmt.returning,
        )?;
        let returning_source_schema =
            merge_returning_source_schema(source_rows.row_schema(), returning_source_relation);
        return dml_returning_result_with_projections(
            engine,
            DmlReturningShape {
                table: &target_table,
                target_qualifier: &target_qual,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: Some(&returning_source_schema),
            },
            &projections,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}
