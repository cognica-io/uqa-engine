//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE matching, action execution, and RETURNING projection.

use super::{
    apply_missing_column_defaults, apply_validated_prepared_document_delete,
    apply_validated_prepared_document_rewrite, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, decode_merge_pair, decode_prepared_doc_id,
    decode_prepared_document_delete, decode_prepared_document_rewrite, dml_join_rows,
    dml_null_target_row, dml_returning_result_with_projections, dml_storage_error, dml_target_row,
    document_vectors, encode_merge_pair, encode_prepared_doc_id, encode_prepared_document_delete,
    encode_prepared_document_rewrite, eval_mutation_assignment, eval_mutation_expr,
    expanded_returning_projections, insert_identity_columns, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, lock_physical_mutation_target,
    merge_pair_schema, merge_source_index_value, missing_document_error, partition_insert_target,
    persist_auto_increment_identity, prepare_auto_increment_identity, prepare_document_delete,
    prepare_document_rewrite, prepare_insert_identity, refresh_insert_identity_after_trigger,
    retarget_prepared_document_rewrite, returning_row_context, stage_prepared_document_delete,
    stage_prepared_document_rewrite, update_lock_strength, validate_document_constraints,
    validate_mutation_columns, validate_returning_alias_relations, BTreeMap, BTreeSet, CteScope,
    DmlCommandMutationOverlay, DmlReturningShape, Document, Engine, MergePlan, MergeWhenPlan,
    MutationAssignmentTarget, PhysicalMutationLockTarget, ProjectionPlan, ReturningRowImage,
    ReturningRowImages, SQLError, SQLParam, SQLResult, Value,
};

const MERGE_PREPARED_UPDATE: i64 = 1;
const MERGE_PREPARED_DELETE: i64 = 2;
const MERGE_PREPARED_INSERT: i64 = 3;
type MergeTargetIdentity = (String, uqa_core::DocId);

enum SelectedMergeAction {
    Nothing,
    Update {
        doc_id: uqa_core::DocId,
        old_document: Document,
        new_document: Document,
        updated_columns: Vec<String>,
    },
    Delete {
        doc_id: uqa_core::DocId,
    },
    Insert {
        document: Document,
    },
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_merge(
    engine: &Engine,
    stmt: MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_merge_inner(engine, &stmt, params)
    } else {
        engine.transaction(move |engine| run_merge_inner(engine, &stmt, params))
    }
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_merge_inner(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::truthy;
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
    let target_hierarchy = engine
        .try_table_hierarchy(&target_table)
        .map_err(|error| SQLError::Internal(format!("read MERGE hierarchy: {error}")))?;
    let target_is_partitioned =
        target_hierarchy.partition_spec.is_some() || target_hierarchy.partition_bound.is_some();
    let mut ctes = CteScope::new();
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
    let work_mem = crate::sql::select::physical_work_mem_bytes(engine)?.max(1);
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
            let target_row = dml_target_row(engine, &target_table, &target_qual, *doc_id, &doc)?;
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
                                super::MergePairKind::Matched,
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
                            super::MergePairKind::NotMatchedBySource,
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
                    super::MergePairKind::NotMatchedByTarget,
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
    let action_schema = merge_prepared_action_schema();
    let mut prepared_actions = uqa_execution::SpillBuffer::new(work_mem);
    let mut after_row_events = Vec::new();
    let mut root_deletes = BTreeSet::new();
    let mut has_mutation = false;
    let mut mutated_target_ids = BTreeSet::new();
    let mut referential_actions = super::ReferentialActionContext::default();
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let overlay = DmlCommandMutationOverlay::new(engine);
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
                super::MergePairKind::Matched => {
                    pair.kind = super::MergePairKind::NotMatchedByTarget;
                    pair.storage_table = None;
                    pair.doc_id = None;
                    pair.target_document = None;
                }
                super::MergePairKind::NotMatchedBySource => continue,
                super::MergePairKind::NotMatchedByTarget => {}
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
            (Some(doc_id), Some(document)) => {
                dml_target_row(engine, &target_table, &target_qual, doc_id, document)?
            }
            _ => dml_null_target_row(engine, &target_table, &target_qual)?,
        };
        let mut joined = dml_join_rows(&target_row, &pair.source_row);
        if matches!(pair.kind, super::MergePairKind::Matched)
            && recheck_matches
            && !uqa_sql::expr::truthy(&eval_mutation_expr(
                engine,
                &snapshot_ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?)
        {
            pair.kind = super::MergePairKind::NotMatchedByTarget;
            pair.storage_table = None;
            pair.doc_id = None;
            pair.target_document = None;
            target_row = dml_null_target_row(engine, &target_table, &target_qual)?;
            joined = dml_join_rows(&target_row, &pair.source_row);
        }
        let action_row = match pair.kind {
            super::MergePairKind::Matched => &joined,
            super::MergePairKind::NotMatchedBySource => &target_row,
            super::MergePairKind::NotMatchedByTarget => &pair.source_row,
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
                mut new_document,
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
                new_document = triggered_document;
                let destination_table = if target_is_partitioned {
                    // PostgreSQL's ONLY modifier limits target matching, not the partition routing performed by an action.
                    partition_insert_target(engine, &target_table, &new_document, params, true)?
                } else {
                    storage_table.to_string()
                };
                let mut prepared = prepare_document_rewrite(
                    engine,
                    storage_table,
                    doc_id,
                    old_document,
                    new_document,
                    params,
                    &mut referential_actions,
                )?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "MERGE rewrite dependency tree was cyclic at its root".into(),
                    )
                })?;
                retarget_prepared_document_rewrite(engine, &mut prepared, &destination_table)?;
                let rewritten_doc_id = stage_prepared_document_rewrite(
                    engine,
                    &mut prepared,
                    params,
                    Some(&updated_columns),
                    &mut after_row_events,
                )?;
                if !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: ReturningRowImages {
                                old: Some(ReturningRowImage {
                                    doc_id: prepared.doc_id,
                                    document: &prepared.old_document,
                                }),
                                new: Some(ReturningRowImage {
                                    doc_id: rewritten_doc_id,
                                    document: &prepared.new_document,
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
                affected += 1;
                has_mutation = true;
                push_merge_prepared_action(
                    &mut prepared_actions,
                    &action_schema,
                    MERGE_PREPARED_UPDATE,
                    encode_prepared_document_rewrite(prepared),
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
                    &mut referential_actions,
                    false,
                )?
                .ok_or_else(|| {
                    SQLError::Internal("MERGE delete dependency tree was cyclic at its root".into())
                })?;
                stage_prepared_document_delete(
                    engine,
                    &mut prepared,
                    params,
                    &mut after_row_events,
                )?;
                if !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: ReturningRowImages {
                                old: Some(ReturningRowImage {
                                    doc_id: prepared.doc_id,
                                    document: &prepared.document,
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
                push_merge_prepared_action(
                    &mut prepared_actions,
                    &action_schema,
                    MERGE_PREPARED_DELETE,
                    encode_prepared_document_delete(prepared),
                )?;
            }
            SelectedMergeAction::Insert { mut document } => {
                let (auto_id_col, id_column) =
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
                    },
                )? {
                    after_row_events.push(event);
                }
                if !stmt.returning.is_empty() {
                    returning_rows.push(build_merge_returning_row(
                        engine,
                        MergeReturningRow {
                            target_table: &target_table,
                            target_qual: &target_qual,
                            images: ReturningRowImages {
                                old: None,
                                new: Some(ReturningRowImage {
                                    doc_id,
                                    document: &document,
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
                push_merge_prepared_action(
                    &mut prepared_actions,
                    &action_schema,
                    MERGE_PREPARED_INSERT,
                    encode_merge_prepared_insert(storage_table, doc_id, document),
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
    for prepared in prepared_reader {
        let prepared = prepared.map_err(crate::sql::select::physical_exec_error)?;
        let (action, payload) = decode_merge_prepared_action(prepared)?;
        match action {
            MERGE_PREPARED_UPDATE => {
                let mut prepared = decode_prepared_document_rewrite(payload)?;
                apply_validated_prepared_document_rewrite(engine, &mut prepared)?;
            }
            MERGE_PREPARED_DELETE => {
                let mut prepared = decode_prepared_document_delete(payload)?;
                apply_validated_prepared_document_delete(engine, &mut prepared)?;
            }
            MERGE_PREPARED_INSERT => {
                let (storage_table, doc_id, document) = decode_merge_prepared_insert(payload)?;
                engine.add_prepared_document_with_vector_values(
                    &storage_table,
                    doc_id,
                    document.clone(),
                    document_vectors(engine, &storage_table, &document)?,
                    false,
                )?;
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "MERGE prepared action spill has unknown kind {action}"
                )))
            }
        }
    }
    crate::sql::triggers::fire_after_row_trigger_events(engine, &after_row_events)?;
    referential_actions.fire_after_statement_triggers(engine)?;
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
        if enabled {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &target_table,
                uqa_sql::ast::TriggerTiming::After,
                event,
                columns,
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

fn validate_merge_action_scopes(
    engine: &Engine,
    stmt: &MergePlan,
    target_schema: &uqa_execution::RowSchema,
    source_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let matched_schema =
        uqa_execution::RowSchema::join(target_schema, source_schema, std::iter::empty());
    let expression_type = |expression: &uqa_execution::ScalarExpr,
                           schema: &uqa_execution::RowSchema| {
        uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)
    };
    let validate_boolean = |expression: &uqa_execution::ScalarExpr,
                            schema: &uqa_execution::RowSchema,
                            label: &str|
     -> Result<(), SQLError> {
        if expression_type(expression, schema)?
            .is_some_and(|ty| ty != uqa_sql::ast::ColumnType::Boolean)
        {
            return Err(SQLError::TypeMismatch(format!(
                "argument of {label} must be type boolean"
            )));
        }
        Ok(())
    };
    validate_boolean(&stmt.join_condition, &matched_schema, "MERGE ON")?;
    let has_source_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        )
    });
    let has_target_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. }
        )
    });
    if has_source_missing
        && has_target_missing
        && !crate::sql::from_rows::join_conjuncts(&stmt.join_condition)
            .into_iter()
            .any(|conjunct| {
                matches!(
                    conjunct,
                    uqa_execution::ScalarExpr::Binary {
                        op: uqa_sql::ast::BinaryOp::Equal,
                        lhs,
                        rhs,
                    } if crate::sql::from_rows::decide_join_sides(
                        target_schema,
                        source_schema,
                        lhs,
                        rhs,
                    )
                    .is_some()
                )
            })
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message:
                "FULL JOIN is only supported with merge-joinable or hash-joinable join conditions"
                    .into(),
        });
    }
    for clause in &stmt.when_clauses {
        let (condition, expressions, schema): (
            Option<&uqa_execution::ScalarExpr>,
            Vec<&uqa_execution::ScalarExpr>,
            &uqa_execution::RowSchema,
        ) = match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                &matched_schema,
            ),
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                (condition.as_ref(), Vec::new(), &matched_schema)
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                target_schema,
            ),
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                (condition.as_ref(), Vec::new(), target_schema)
            }
            MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => (condition.as_ref(), values.iter().collect(), source_schema),
            MergeWhenPlan::NothingNotMatched { condition } => {
                (condition.as_ref(), Vec::new(), source_schema)
            }
        };
        if let Some(condition) = condition {
            validate_boolean(condition, schema, "WHEN")?;
        }
        for expression in expressions {
            expression_type(expression, schema)?;
        }
    }
    Ok(())
}

fn ensure_merge_target_is_modified_once(
    mutated_target_ids: &mut BTreeSet<MergeTargetIdentity>,
    storage_table: &str,
    doc_id: uqa_core::DocId,
) -> Result<(), SQLError> {
    if mutated_target_ids.insert((storage_table.to_string(), doc_id)) {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "21000".into(),
        message: "MERGE command cannot affect row a second time".into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn select_merge_action(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
    match_kind: super::MergePairKind,
    doc_id: Option<uqa_core::DocId>,
    target_document: Option<&Document>,
    action_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SelectedMergeAction, SQLError> {
    for clause in &stmt.when_clauses {
        let (condition, applies) = match clause {
            MergeWhenPlan::UpdateMatched { condition, .. }
            | MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition }
                if matches!(match_kind, super::MergePairKind::Matched) =>
            {
                (condition.as_ref(), true)
            }
            MergeWhenPlan::InsertNotMatched { condition, .. }
            | MergeWhenPlan::NothingNotMatched { condition }
                if matches!(match_kind, super::MergePairKind::NotMatchedByTarget) =>
            {
                (condition.as_ref(), true)
            }
            MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition }
                if matches!(match_kind, super::MergePairKind::NotMatchedBySource) =>
            {
                (condition.as_ref(), true)
            }
            _ => (None, false),
        };
        if !applies {
            continue;
        }
        if let Some(condition) = condition {
            let value = eval_mutation_expr(engine, ctes, condition, Some(action_row), params)?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
        }
        return match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let doc_id = doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE update lost its target identity".into())
                })?;
                let old_document = target_document
                    .cloned()
                    .ok_or_else(|| missing_document_error("MERGE update", target_table, doc_id))?;
                let mut new_document = old_document.clone();
                for assignment in assignments {
                    let value = eval_mutation_assignment(
                        engine,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            column: &assignment.column,
                            action: "MERGE UPDATE",
                        },
                        &assignment.value,
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        new_document.insert(assignment.column.clone(), value);
                    } else {
                        new_document.remove(&assignment.column);
                    }
                }
                Ok(SelectedMergeAction::Update {
                    doc_id,
                    old_document,
                    new_document,
                    updated_columns: assignments
                        .iter()
                        .map(|assignment| assignment.column.clone())
                        .collect(),
                })
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => Ok(SelectedMergeAction::Delete {
                doc_id: doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE delete lost its target identity".into())
                })?,
            }),
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                let implicit_columns = columns.is_empty();
                let target_columns = if implicit_columns {
                    engine
                        .try_table_columns(target_table)
                        .map_err(|error| dml_storage_error("MERGE INSERT", error))?
                } else {
                    columns.clone()
                };
                if values.len() > target_columns.len()
                    || (!implicit_columns && values.len() != target_columns.len())
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "MERGE INSERT row width {} != column count {}",
                        values.len(),
                        target_columns.len()
                    )));
                }
                validate_mutation_columns(
                    engine,
                    target_table,
                    target_columns.iter().map(String::as_str),
                    "MERGE INSERT",
                )?;
                let mut document = Document::new();
                for (index, column) in target_columns.iter().take(values.len()).enumerate() {
                    let value = eval_mutation_assignment(
                        engine,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            column,
                            action: "MERGE INSERT",
                        },
                        &values[index],
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        document.insert(column.clone(), value);
                    }
                }
                apply_missing_column_defaults(engine, target_table, &mut document, params)?;
                Ok(SelectedMergeAction::Insert { document })
            }
            MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => Ok(SelectedMergeAction::Nothing),
        };
    }
    Ok(SelectedMergeAction::Nothing)
}

fn merge_prepared_action_schema() -> uqa_execution::RowSchema {
    uqa_execution::RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![Some(uqa_sql::ast::ColumnType::BigInteger), None],
    )
}

fn push_merge_prepared_action(
    buffer: &mut uqa_execution::SpillBuffer,
    schema: &uqa_execution::RowSchema,
    action: i64,
    payload: Value,
) -> Result<(), SQLError> {
    buffer
        .push(uqa_execution::Batch::from_physical_rows(
            schema.clone(),
            vec![uqa_execution::PhysicalRow::from_values(vec![
                Value::Int(action),
                payload,
            ])],
        ))
        .map_err(crate::sql::select::physical_exec_error)?;
    Ok(())
}

fn decode_merge_prepared_action(
    row: uqa_execution::OwnedPhysicalRow,
) -> Result<(i64, Value), SQLError> {
    let action = match row.physical_value_at(0) {
        Some(Value::Int(action)) => *action,
        _ => {
            return Err(SQLError::Internal(
                "MERGE prepared action spill lost its action kind".into(),
            ))
        }
    };
    let payload = row
        .physical_value_at(1)
        .cloned()
        .ok_or_else(|| SQLError::Internal("MERGE prepared action spill lost its payload".into()))?;
    Ok((action, payload))
}

fn encode_merge_prepared_insert(
    storage_table: String,
    doc_id: uqa_core::DocId,
    document: Document,
) -> Value {
    Value::List(vec![
        Value::Str(storage_table),
        encode_prepared_doc_id(doc_id),
        Value::Map(document),
    ])
}

fn decode_merge_prepared_insert(
    value: Value,
) -> Result<(String, uqa_core::DocId, Document), SQLError> {
    let Value::List(fields) = value else {
        return Err(SQLError::Internal(
            "MERGE prepared insert payload is not a record".into(),
        ));
    };
    let [storage_table, doc_id, document]: [Value; 3] =
        fields.try_into().map_err(|fields: Vec<Value>| {
            SQLError::Internal(format!(
                "MERGE prepared insert payload has {} fields, expected 3",
                fields.len()
            ))
        })?;
    let storage_table = match storage_table {
        Value::Str(table) => table,
        _ => {
            return Err(SQLError::Internal(
                "MERGE prepared insert payload has no storage table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(doc_id, "MERGE prepared insert")?;
    let document = match document {
        Value::Map(document) => document,
        _ => {
            return Err(SQLError::Internal(
                "MERGE prepared insert payload has no document".into(),
            ))
        }
    };
    Ok((storage_table, doc_id, document))
}

fn merge_target_lock_strength(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
) -> uqa_sql::ast::LockStrength {
    if stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        )
    }) {
        return uqa_sql::ast::LockStrength::ForUpdate;
    }
    let columns = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => Some(assignments),
            _ => None,
        })
        .flatten()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    if columns.is_empty() {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        update_lock_strength(engine, target_table, &columns)
    }
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct MergeReturningRow<'a> {
    target_table: &'a str,
    target_qual: &'a str,
    images: ReturningRowImages<'a>,
    returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    source_row: &'a uqa_execution::OwnedPhysicalRow,
    source_schema: &'a uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    action: &'a str,
}

pub(in crate::sql) fn build_merge_returning_row(
    engine: &Engine,
    input: MergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let row = merge_returning_context(engine, input)?;
    let projections = expanded_merge_returning_projections(
        engine,
        input.target_table,
        input.target_qual,
        input.returning_aliases,
        input.source_schema,
        input.source_relation,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

fn merge_returning_context(
    engine: &Engine,
    input: MergeReturningRow<'_>,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let mut row = returning_row_context(
        engine,
        input.target_table,
        input.target_qual,
        input.images,
        input.returning_aliases,
    )?;
    row.schema = uqa_execution::RowSchema::append_internal_typed(
        &row.schema,
        &[(
            crate::sql::merge_action_attribute(),
            Some(uqa_sql::ast::ColumnType::Text),
        )],
    );
    row.row = row.row.append_values(vec![Value::Str(input.action.into())]);
    let aliases = input
        .source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            let slot = input
                .source_row
                .schema
                .physical_slot(position)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "MERGE RETURNING source lost physical column {position}"
                    ))
                })?;
            Ok((
                input.source_relation.column(position),
                slot,
                input.source_schema.column_type(position).cloned(),
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let source_schema = uqa_execution::RowSchema::with_physical_internal_aliases(
        &input.source_row.schema,
        &aliases,
    );
    row = uqa_execution::OwnedPhysicalRow::new(
        uqa_execution::RowSchema::join(&row.schema, &source_schema, std::iter::empty()),
        uqa_execution::PhysicalRow::concat(&row.row, &input.source_row.row),
    );
    Ok(row)
}

fn merge_returning_source_schema(
    source_schema: &uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
) -> uqa_execution::RowSchema {
    let aliases = source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                source_relation.column(position),
                source_schema
                    .physical_slot(position)
                    .expect("source column has a physical slot"),
                source_schema.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    uqa_execution::RowSchema::with_physical_internal_aliases(source_schema, &aliases)
}

fn expanded_merge_returning_projections(
    engine: &Engine,
    target_table: &str,
    target_qualifier: &str,
    aliases: &uqa_sql::ast::ReturningAliases,
    source_schema: &uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let target_star = ProjectionPlan {
        expr: uqa_execution::ScalarExpr::QualifiedStar(target_qualifier.into()),
        alias: None,
    };
    let target_projections = expanded_returning_projections(
        engine,
        target_table,
        target_qualifier,
        aliases,
        std::slice::from_ref(&target_star),
    )?;
    let mut projections = Vec::new();
    for projection in returning {
        match &projection.expr {
            uqa_execution::ScalarExpr::Star => {
                projections.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            crate::sql::select::visible_projection_source_position(
                                source_schema,
                                *position,
                            )
                        })
                        .map(|(position, column)| ProjectionPlan {
                            expr: uqa_execution::ScalarExpr::InternalColumn(
                                source_relation.column(position),
                            ),
                            alias: Some(
                                source_schema
                                    .public_name(position)
                                    .unwrap_or(column)
                                    .to_string(),
                            ),
                        }),
                );
                projections.extend(target_projections.iter().cloned());
            }
            uqa_execution::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(expanded_returning_projections(
                    engine,
                    target_table,
                    target_qualifier,
                    aliases,
                    std::slice::from_ref(projection),
                )?);
            }
            _ => projections.push(projection.clone()),
        }
    }
    Ok(projections)
}
