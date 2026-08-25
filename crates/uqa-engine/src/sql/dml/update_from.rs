//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE FROM join-source execution.

use super::{
    apply_validated_prepared_document_rewrite, build_join_spill_with_ctes, build_returning_row,
    dml_join_rows, dml_returning_result, dml_target_row, eval_mutation_assignment,
    eval_mutation_expr, finalize_partition_rewrite, lock_physical_mutation_target,
    prepare_document_rewrite, stage_prepared_document_rewrite, update_lock_strength,
    validate_returning_alias_relations, CteScope, DmlCommandMutationOverlay, DmlReturningShape,
    Engine, MutationAssignmentTarget, PartitionRewritePolicy, PhysicalMutationLockTarget,
    ReturningProjectionRow, ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult,
    SourcePlan, UpdatePlan,
};

pub(in crate::sql) fn run_update_from(
    engine: &Engine,
    stmt: &UpdatePlan,
    from_clause: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    let from_rows = build_join_spill_with_ctes(engine, from_clause, params, ctes)?;
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
    let target_tables = engine.hierarchy_scan_tables(&target, stmt.include_descendants)?;
    let mut target_rows = Vec::new();
    for table in target_tables {
        target_rows.extend(
            engine
                .table_doc_ids(&table)?
                .into_iter()
                .map(|doc_id| (table.clone(), doc_id)),
        );
    }
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let overlay = DmlCommandMutationOverlay::new(engine);
    let mut prepared_updates = Vec::new();
    let mut rewrite_stack = Vec::new();
    let mut locked_ids = std::collections::BTreeSet::new();
    for (storage_table, doc_id) in target_rows {
        cancel.check()?;
        let Some(candidate) = engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let candidate_row =
            dml_target_row(engine, &target, &stmt.target_qualifier, doc_id, &candidate)?;
        let Some(candidate_source) = matching_update_source(
            engine,
            stmt,
            &snapshot_ctes,
            &from_rows,
            &candidate_row,
            params,
        )?
        else {
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
        let Some(mut doc) = engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let original_doc = doc.clone();
        let target_row = dml_target_row(
            engine,
            &target,
            &stmt.target_qualifier,
            doc_id,
            &original_doc,
        )?;
        let source_context = if recheck {
            update_join_qualifies(
                engine,
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
        // Apply assignments evaluated against the rechecked joined row so RHS expressions cannot consume a target image from before the lock wait.
        for assignment in &stmt.assignments {
            let value = eval_mutation_assignment(
                engine,
                &snapshot_ctes,
                MutationAssignmentTarget {
                    table: &target,
                    column: &assignment.column,
                    action: "UPDATE FROM",
                },
                &assignment.value,
                Some(&joined),
                params,
            )?;
            if let Some(value) = value {
                doc.insert(assignment.column.clone(), value);
            } else {
                doc.remove(&assignment.column);
            }
        }
        let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
            engine,
            &storage_table,
            uqa_sql::ast::TriggerEvent::Update,
            doc_id,
            Some(&original_doc),
            Some(&doc),
            &assigned_columns,
        )?
        else {
            continue;
        };
        doc = triggered_document;
        if let Some(mut prepared) = prepare_document_rewrite(
            engine,
            &storage_table,
            doc_id,
            original_doc,
            doc,
            params,
            &mut rewrite_stack,
        )? {
            finalize_partition_rewrite(
                engine,
                &mut prepared,
                &target,
                params,
                stmt.include_descendants,
                PartitionRewritePolicy::Move,
            )?;
            let rewritten_doc_id = stage_prepared_document_rewrite(engine, &mut prepared, params)?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    ReturningProjectionRow {
                        table: &target,
                        target_qualifier: &stmt.target_qualifier,
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
                        aliases: &stmt.returning_aliases,
                        context: Some(&source_context),
                    },
                    &stmt.returning,
                    params,
                    &snapshot_ctes,
                )?);
            }
            affected += 1;
            prepared_updates.push((prepared, source_context, rewritten_doc_id));
        }
    }
    drop(overlay);
    if !prepared_updates.is_empty() {
        engine.prepare_explicit_transaction_writer()?;
        for (prepared, _, _) in &mut prepared_updates {
            apply_validated_prepared_document_rewrite(engine, prepared)?;
        }
    }
    for (prepared, _, rewritten_doc_id) in &prepared_updates {
        crate::sql::triggers::fire_after_row_trigger_event(
            engine,
            crate::sql::triggers::AfterRowTriggerEvent::new(
                &prepared.table,
                uqa_sql::ast::TriggerEvent::Update,
                prepared.doc_id,
                *rewritten_doc_id,
                Some(&prepared.old_document),
                Some(&prepared.new_document),
                &assigned_columns,
            ),
        )?;
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            DmlReturningShape {
                table: &target,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes,
                supplemental_schema: Some(from_rows.row_schema()),
            },
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

fn matching_update_source(
    engine: &Engine,
    stmt: &UpdatePlan,
    ctes: &CteScope,
    from_rows: &uqa_execution::SharedSpill,
    target_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<Option<uqa_execution::OwnedPhysicalRow>, SQLError> {
    let from_reader = from_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for from_row in from_reader {
        let source_context = from_row.map_err(crate::sql::select::physical_exec_error)?;
        if update_join_qualifies(engine, stmt, ctes, target_row, &source_context, params)? {
            return Ok(Some(source_context));
        }
    }
    Ok(None)
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
