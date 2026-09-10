//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table MERGE pairing, lock rechecks, row staging, publication, and trigger ordering.
use super::{
    actions::{ensure_merge_target_is_modified_once, select_merge_action},
    analysis::merge_target_lock_strength,
    codec::{
        decode_merge_pair, decode_prepared_mutation_action_row, encode_merge_pair,
        merge_pair_schema, merge_source_index_value, prepared_mutation_action_schema,
        push_prepared_mutation_action, MergePairKind,
    },
    model::{MergeTargetIdentity, SelectedMergeAction},
    returning::{build_merge_returning_row, MergeReturningRow},
};
use crate::mutation::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::{
    mutation::{
        assignment::{refresh_stored_generated_columns, validate_view_checks, ViewCheckContext},
        candidate::PhysicalMutationLockTarget,
        command_scope::MutationOverlayScope,
        constraints::{
            lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
            validate_document_constraints,
        },
        errors::{dml_storage_error, missing_document_error},
        expressions::eval_mutation_expr,
        identity::{
            insert_identity_columns, integer_primary_key_doc_id, persist_auto_increment_identity,
            prepare_auto_increment_identity, prepare_insert_identity,
            refresh_insert_identity_after_trigger, IdentityAllocationContext,
        },
        locking::lock_physical_mutation_target,
        prepared::{PreparedDocumentInsert, PreparedMutationAction},
        publication::{
            finish_mutation_publication, publish_prepared_mutation_action, MutationPublicationBatch,
        },
        referential::{
            prepare_document_delete, prepare_partition_update_route,
            prepare_routed_document_rewrite,
        },
        returning::{dml_returning_result_with_projections, DmlReturningShape},
        row_images::{MutationRowImage, MutationRowImages},
        rows::{
            existing_tuple_metadata, join_rows as dml_join_rows, new_tuple_metadata,
            null_target_row as dml_null_target_row,
            target_row_for_storage as dml_target_row_for_storage,
        },
        staging::{stage_prepared_document_delete, stage_prepared_document_rewrite},
    },
    query::{sources::build_join_spill_with_ctes, CteScope},
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    plan::{MergePlan, MergeWhenPlan},
    semantics::{
        merge::{
            expanded_merge_returning_projections, merge_returning_source_schema,
            validate_merge_action_scopes,
        },
        partition::partition_insert_target,
        returning::validate_returning_alias_relations,
    },
    SQLError, SQLParam, SQLResult,
};

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub fn run_table_merge<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    stmt: &MergePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::truthy;
    let mutation = &context.mutation;
    let preparation = mutation.preparation;
    let referential = &preparation.referential;
    let assignment = referential.assignment;
    let constraints = referential.constraints;
    let triggers = referential.triggers;
    super::analysis::ensure_merge_privileges(mutation, stmt, inherited_ctes)?;
    let _transition_capture_scope = crate::mutation::triggers::TransitionCaptureScope::enter();
    let target_table = stmt.target.clone();
    referential.locking.session.lock_relation(
        &target_table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    if [
        uqa_sql::ast::RuleEvent::Insert,
        uqa_sql::ast::RuleEvent::Update,
        uqa_sql::ast::RuleEvent::Delete,
    ]
    .into_iter()
    .map(|event| {
        mutation
            .rules
            .rules
            .analysis
            .rules
            .rules_for(&target_table, event)
    })
    .collect::<Result<Vec<_>, SQLError>>()?
    .iter()
    .any(|rules| !rules.is_empty())
    {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(&target_table).map_err(|error| {
                SQLError::Internal(format!("decode MERGE relation `{target_table}`: {error}"))
            })?;
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot execute MERGE on relation \"{}\"", relation.name),
        });
    }
    let target_qual = stmt.target_qualifier.clone();
    let target_tables = referential
        .locking
        .catalog
        .hierarchy_scan_tables(&target_table, stmt.include_descendants)?;
    let statement_events = super::statement_events::MergeStatementEvents::from_plan(stmt);
    let mut ctes = mutation
        .scopes
        .command_scope(stmt.statement_privilege_subject.as_deref(), false)?;
    if let Some(parent) = inherited_ctes {
        ctes.inherit_cte_bindings(parent);
    }
    if ctes.command_cte_snapshot().is_none()
        && (stmt.ctes.iter().any(|cte| cte.body.modifies_data())
            || statement_events.has_before_statement_trigger(&triggers, &target_table)?)
    {
        ctes.set_command_cte_snapshot(Some(std::sync::Arc::new(context.snapshots.capture()?)));
    }
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    uqa_sql::semantics::merge::validate_merge_target_columns(assignment.columns, stmt)?;
    let statement_snapshot = ctes.command_cte_snapshot();
    let mut execute_read =
        |read_context: &MutationStatementContext<'_, S>| -> Result<SQLResult, SQLError> {
            let read_referential = &read_context.mutation.preparation.referential;
            let read_assignment = read_referential.assignment;
            let analysis_scope =
                super::analysis::merge_analysis_scope(mutation.scopes, stmt, inherited_ctes)?;
            let source_schema = crate::query::binding::analyze_source_plan_schema(
                read_context.query.source.ctes.routines,
                &stmt.source,
                params,
                &analysis_scope,
                None,
            )?;
            let returning_source_relation = uqa_sql::ast::InternalRelationId::allocate();
            validate_returning_alias_relations(
                &target_qual,
                &stmt.returning_aliases,
                Some(&source_schema),
            )?;
            let null_target_row =
                dml_null_target_row(assignment.rows, &target_table, &target_qual)?;
            validate_merge_action_scopes(
                preparation.returning.routines,
                stmt,
                &null_target_row.schema,
                &source_schema,
                params,
                &crate::query::binding::binding_context(&analysis_scope)?,
            )?;
            uqa_sql::semantics::merge::merge_returning_schema(
                preparation.returning.routines,
                preparation.returning.catalog,
                stmt,
                params,
                &source_schema,
                &crate::query::binding::binding_context(&analysis_scope)?,
            )?;
            statement_events.fire_before(&triggers, &target_table)?;
            crate::query::cte::materialize_plan_ctes(
                context.query.source.ctes,
                &stmt.ctes,
                params,
                &mut ctes,
            )?;
            let source_rows = build_join_spill_with_ctes(
                &read_context.query.source,
                &stmt.source,
                params,
                &mut ctes,
            )?;
            let mut affected = 0_u64;
            let mut returning_rows = Vec::new();

            let pair_schema = merge_pair_schema(source_rows.row_schema());
            let work_mem = crate::query::projection::physical_work_mem_bytes(
                context.query.source.relational.runtime,
            )?
            .max(1);
            let mut pairings = crate::SpillBuffer::new(work_mem);
            let mut matched_source = crate::ExactRowSet::new(work_mem);
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
            let null_source_row = crate::OwnedPhysicalRow::new(
                source_rows.row_schema().clone(),
                crate::PhysicalRow::nulls(source_rows.row_schema().physical_width()),
            );

            for storage_table in &target_tables {
                for doc_id in &read_referential
                    .constraints
                    .reads
                    .table_doc_ids(storage_table)?
                {
                    let Some(doc) = read_referential
                        .constraints
                        .reads
                        .get_document(storage_table, *doc_id)?
                    else {
                        return Err(missing_document_error("MERGE scan", storage_table, *doc_id));
                    };
                    let target_row = dml_target_row_for_storage(
                        read_assignment.rows,
                        &target_table,
                        storage_table,
                        &target_qual,
                        *doc_id,
                        &doc,
                    )?;
                    if let Some(predicate) = &stmt.target_predicate {
                        if !truthy(&eval_mutation_expr(
                            read_assignment.expressions,
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
                        .map_err(crate::physical::physical_exec_error)?;
                    for (idx, src) in source_reader.enumerate() {
                        let src = src.map_err(crate::physical::physical_exec_error)?;
                        let joined = dml_join_rows(&target_row, &src);
                        if truthy(&eval_mutation_expr(
                            read_assignment.expressions,
                            &ctes,
                            &stmt.join_condition,
                            Some(&joined),
                            params,
                        )?) {
                            target_matched = true;
                            let index_value = merge_source_index_value(idx);
                            let _ = matched_source
                                .insert_values(std::slice::from_ref(&index_value))
                                .map_err(crate::physical::physical_exec_error)?;
                            pairings
                                .push(crate::Batch::from_physical_rows(
                                    pair_schema.clone(),
                                    vec![encode_merge_pair(
                                        MergePairKind::Matched,
                                        Some(storage_table),
                                        Some(*doc_id),
                                        Some(&doc),
                                        &src,
                                    )],
                                ))
                                .map_err(crate::physical::physical_exec_error)?;
                        }
                    }
                    if target_matched && matched_can_mutate {
                        lock_target_ids.insert((storage_table.clone(), *doc_id));
                    } else if !target_matched && has_source_missing_clause {
                        if source_missing_can_mutate {
                            lock_target_ids.insert((storage_table.clone(), *doc_id));
                        }
                        pairings
                            .push(crate::Batch::from_physical_rows(
                                pair_schema.clone(),
                                vec![encode_merge_pair(
                                    MergePairKind::NotMatchedBySource,
                                    Some(storage_table),
                                    Some(*doc_id),
                                    Some(&doc),
                                    &null_source_row,
                                )],
                            ))
                            .map_err(crate::physical::physical_exec_error)?;
                    }
                }
            }
            let source_reader = source_rows
                .read_rows()
                .map_err(crate::physical::physical_exec_error)?;
            for (idx, src) in source_reader.enumerate() {
                let src = src.map_err(crate::physical::physical_exec_error)?;
                let index_value = merge_source_index_value(idx);
                if matched_source
                    .contains_values(std::slice::from_ref(&index_value))
                    .map_err(crate::physical::physical_exec_error)?
                {
                    continue;
                }
                pairings
                    .push(crate::Batch::from_physical_rows(
                        pair_schema.clone(),
                        vec![encode_merge_pair(
                            MergePairKind::NotMatchedByTarget,
                            None,
                            None,
                            None,
                            &src,
                        )],
                    ))
                    .map_err(crate::physical::physical_exec_error)?;
            }

            let pairings = pairings
                .into_shared(pair_schema)
                .map_err(crate::physical::physical_exec_error)?;
            let mut recheck_matches = false;
            // A paired target may have been moved to a successor identity by a primary-key rewrite another transaction committed while this statement waited; PostgreSQL 18 follows the update chain, so the pairing is redirected to the successor before the actions run.
            let mut successors: BTreeMap<MergeTargetIdentity, MergeTargetIdentity> =
                BTreeMap::new();
            let mut rechecked_target_ids = BTreeSet::new();
            let mut deleted_targets = BTreeSet::new();
            for (storage_table, doc_id) in lock_target_ids {
                let original_identity = (storage_table.clone(), doc_id);
                let target = lock_physical_mutation_target(
                    referential.locking.session,
                    &storage_table,
                    &target_qual,
                    doc_id,
                    merge_target_lock_strength(referential.locking.catalog, stmt, &storage_table),
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
                constraints
                    .transactions
                    .refresh_explicit_statement_snapshot()?;
            }
            let mut refreshed_targets = BTreeMap::new();
            for original_identity in rechecked_target_ids {
                let locked_identity = successors
                    .get(&original_identity)
                    .cloned()
                    .unwrap_or_else(|| original_identity.clone());
                if let Some(document) = constraints
                    .reads
                    .get_document(&locked_identity.0, locked_identity.1)?
                {
                    refreshed_targets.insert(original_identity, (locked_identity, document));
                } else {
                    deleted_targets.insert(original_identity);
                }
            }
            let action_schema = prepared_mutation_action_schema();
            let mut prepared_actions = crate::SpillBuffer::new(work_mem);
            let mut events = crate::mutation::events::MutationEventQueue::default();
            let mut root_deletes = BTreeSet::new();
            let mut has_mutation = false;
            let mut mutated_target_ids = BTreeSet::new();
            let snapshot_ctes = ctes.returning_statement_snapshot_scope();
            let overlay = MutationOverlayScope::new(mutation.state);
            let pairing_reader = pairings
                .read_rows()
                .map_err(crate::physical::physical_exec_error)?;
            for pair in pairing_reader {
                let pair = pair.map_err(crate::physical::physical_exec_error)?;
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
                } else if let Some(((successor_table, successor_doc_id), document)) =
                    original_identity
                        .as_ref()
                        .and_then(|identity| refreshed_targets.get(identity))
                {
                    pair.storage_table = Some(successor_table.clone());
                    pair.doc_id = Some(*successor_doc_id);
                    pair.target_document = Some(document.clone());
                }
                let mut target_row = match (pair.doc_id, pair.target_document.as_ref()) {
                    (Some(doc_id), Some(document)) => dml_target_row_for_storage(
                        assignment.rows,
                        &target_table,
                        pair.storage_table.as_deref().ok_or_else(|| {
                            SQLError::Internal("MERGE target row lost its physical relation".into())
                        })?,
                        &target_qual,
                        doc_id,
                        document,
                    )?,
                    _ => dml_null_target_row(assignment.rows, &target_table, &target_qual)?,
                };
                let mut joined = dml_join_rows(&target_row, &pair.source_row);
                if recheck_matches && pair.doc_id.is_some() {
                    let target_visible = match &stmt.target_predicate {
                        Some(predicate) => truthy(&eval_mutation_expr(
                            assignment.expressions,
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
                                target_row = dml_null_target_row(
                                    assignment.rows,
                                    &target_table,
                                    &target_qual,
                                )?;
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
                        assignment.expressions,
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
                    target_row = dml_null_target_row(assignment.rows, &target_table, &target_qual)?;
                    joined = dml_join_rows(&target_row, &pair.source_row);
                }
                let action_row = match pair.kind {
                    MergePairKind::Matched => &joined,
                    MergePairKind::NotMatchedBySource => &target_row,
                    MergePairKind::NotMatchedByTarget => &pair.source_row,
                };
                match select_merge_action(
                    assignment,
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
                        let Some(triggered_document) =
                            crate::mutation::triggers::fire_before_row_triggers(
                                &triggers,
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
                            referential,
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
                            referential,
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
                        let primary_key_doc_id = integer_primary_key_doc_id(
                            constraints.catalog,
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
                            services: assignment,
                            table: &target_table,
                            storage_table: &new_storage_table,
                            target_qualifier: &target_qual,
                            doc_id: checked_doc_id,
                            document: &prepared.new_document,
                            checks: &stmt.view_checks,
                            params,
                            scope: &snapshot_ctes,
                        })?;
                        let old_metadata = existing_tuple_metadata(
                            assignment.rows,
                            &prepared.table,
                            prepared.doc_id,
                        )?;
                        let new_metadata = new_tuple_metadata(assignment.rows)?;
                        let rewritten_doc_id = stage_prepared_document_rewrite(
                            preparation.staging,
                            &mut prepared,
                            params,
                            Some(&updated_columns),
                            events.after_rows_mut(),
                        )?;
                        if row_affected && !stmt.returning.is_empty() {
                            returning_rows.push(build_merge_returning_row(
                                &preparation.returning,
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
                        let old_document = pair.target_document.as_ref().ok_or_else(|| {
                            SQLError::Internal("MERGE delete lost its target row".into())
                        })?;
                        if crate::mutation::triggers::fire_before_row_triggers(
                            &triggers,
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
                            referential,
                            storage_table,
                            doc_id,
                            params,
                            &root_deletes,
                            events.referential_actions_mut(),
                            false,
                        )?
                        .ok_or_else(|| {
                            SQLError::Internal(
                                "MERGE delete dependency tree was cyclic at its root".into(),
                            )
                        })?;
                        let old_metadata = existing_tuple_metadata(
                            assignment.rows,
                            &prepared.table,
                            prepared.doc_id,
                        )?;
                        stage_prepared_document_delete(
                            preparation.staging,
                            &mut prepared,
                            params,
                            events.after_rows_mut(),
                        )?;
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_merge_returning_row(
                                &preparation.returning,
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
                            insert_identity_columns(
                                mutation.identities,
                                &target_table,
                                "MERGE INSERT",
                            )?;
                        let prepared_auto_identity = prepare_auto_increment_identity(
                            mutation.identities,
                            &target_table,
                            &id_column,
                            auto_id_col.as_deref(),
                            &mut document,
                            "prepare MERGE INSERT identity",
                        )?;
                        // MERGE INTO ONLY excludes descendants from matching, while PostgreSQL still routes INSERT actions through the target's partition tree.
                        let storage_table = partition_insert_target(
                            &constraints.partitions,
                            &target_table,
                            &document,
                            params,
                            true,
                        )?;
                        referential.locking.session.lock_relation(
                            &storage_table,
                            crate::row_locks::RelationLockMode::RowExclusive,
                        )?;
                        let mut insert_identity = match prepared_auto_identity {
                            Some(identity) => identity,
                            None => prepare_insert_identity(
                                mutation.identities,
                                &storage_table,
                                &id_column,
                                accepts_supplied_identity,
                                None,
                                &mut document,
                                "prepare MERGE INSERT identity",
                            )?,
                        };
                        let doc_id = insert_identity.0;
                        let Some(triggered_document) =
                            crate::mutation::triggers::fire_before_row_triggers(
                                &triggers,
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
                        refresh_stored_generated_columns(
                            assignment,
                            &storage_table,
                            &mut document,
                        )?;
                        refresh_insert_identity_after_trigger(
                            IdentityAllocationContext {
                                identifiers: mutation.identities.identifiers,
                                partitions: mutation.identities.partitions,
                            },
                            &storage_table,
                            &id_column,
                            accepts_supplied_identity,
                            auto_id_col.as_deref(),
                            &document,
                            &mut insert_identity,
                        )?;
                        let doc_id = insert_identity.0;
                        let trigger_target = partition_insert_target(
                            &constraints.partitions,
                            &target_table,
                            &document,
                            params,
                            true,
                        )?;
                        if trigger_target != storage_table {
                            return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "moving row to another partition during a BEFORE FOR EACH ROW trigger is not supported".into(),
                    });
                        }
                        lock_existing_document_foreign_key_dependencies(
                            constraints,
                            &storage_table,
                            &document,
                        )?;
                        let _key_locks = lock_document_key_dependencies(
                            constraints,
                            &storage_table,
                            &document,
                            None,
                        )?;
                        validate_document_constraints(
                            constraints,
                            &storage_table,
                            &document,
                            params,
                            None,
                        )?;
                        validate_view_checks(ViewCheckContext {
                            services: assignment,
                            table: &target_table,
                            storage_table: &storage_table,
                            target_qualifier: &target_qual,
                            doc_id,
                            document: &document,
                            checks: &stmt.view_checks,
                            params,
                            scope: &snapshot_ctes,
                        })?;
                        preparation.staging.commands.stage_command_document(
                            &storage_table,
                            doc_id,
                            Some(document.clone()),
                        )?;
                        if let Some(event) =
                            crate::mutation::triggers::AfterRowTriggerEvent::prepare(
                                &triggers,
                                crate::mutation::triggers::AfterRowTriggerInput {
                                    table: &storage_table,
                                    event: uqa_sql::ast::TriggerEvent::Insert,
                                    old_doc_id: doc_id,
                                    new_doc_id: doc_id,
                                    old_document: None,
                                    new_document: Some(&document),
                                    updated_columns: &[],
                                    cascade_parent: None,
                                },
                            )?
                        {
                            crate::mutation::triggers::AfterRowTriggerEvent::push(
                                events.after_rows_mut(),
                                event,
                            );
                        }
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_merge_returning_row(
                                &preparation.returning,
                                MergeReturningRow {
                                    target_table: &target_table,
                                    target_qual: &target_qual,
                                    images: MutationRowImages {
                                        old: None,
                                        new: Some(MutationRowImage {
                                            storage_table: storage_table.clone(),
                                            doc_id,
                                            document: &document,
                                            metadata: new_tuple_metadata(assignment.rows)?,
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
                .map_err(crate::physical::physical_exec_error)?;
            if has_mutation {
                mutation.state.prepare_writer()?;
                let auto_id_column = mutation
                    .identities
                    .catalog
                    .auto_increment_column(&target_table)
                    .map_err(|error| dml_storage_error("MERGE INSERT", error))?;
                persist_auto_increment_identity(
                    mutation.identities,
                    &target_table,
                    auto_id_column.as_deref(),
                    "persist MERGE INSERT identity",
                )?;
            }
            let prepared_reader = prepared_actions
                .read_rows()
                .map_err(crate::physical::physical_exec_error)?;
            let mut publication = MutationPublicationBatch::default();
            for prepared in prepared_reader {
                let prepared = prepared.map_err(crate::physical::physical_exec_error)?;
                let action = decode_prepared_mutation_action_row(prepared)?;
                publish_prepared_mutation_action(
                    mutation.publication,
                    action,
                    false,
                    &mut publication,
                )?;
            }
            finish_mutation_publication(mutation.publication, &mut publication)?;
            statement_events.fire_table_after(&triggers, &target_table, &events)?;
            if !stmt.returning.is_empty() {
                let projections = expanded_merge_returning_projections(
                    preparation.returning.catalog,
                    &target_table,
                    &target_qual,
                    &stmt.returning_aliases,
                    source_rows.row_schema(),
                    returning_source_relation,
                    &stmt.returning,
                )?;
                let returning_source_schema = merge_returning_source_schema(
                    source_rows.row_schema(),
                    returning_source_relation,
                );
                return dml_returning_result_with_projections(
                    preparation.returning,
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
        };
    match statement_snapshot.as_deref() {
        Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute_read),
        None => execute_read(context),
    }
}
