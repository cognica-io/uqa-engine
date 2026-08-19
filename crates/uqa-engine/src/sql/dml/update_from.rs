//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE FROM join-source execution.

use super::{
    apply_prepared_document_rewrite, build_join_spill_with_ctes, build_returning_row,
    dml_join_rows, dml_returning_result, dml_target_row, eval_mutation_assignment,
    eval_mutation_expr, integer_primary_key_doc_id, lock_mutation_target,
    prebuild_locking_returning_row, prepare_document_rewrite, returning_has_row_locks,
    update_lock_strength, validate_returning_alias_relations, CteScope, DmlReturningShape, Engine,
    MutationAssignmentTarget, MutationLockTarget, ReturningProjectionRow, ReturningRowImage,
    ReturningRowImages, SQLError, SQLParam, SQLResult, SourcePlan, UpdatePlan,
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
    let strength = update_lock_strength(
        engine,
        &target,
        &stmt
            .assignments
            .iter()
            .map(|assignment| assignment.column.clone())
            .collect::<Vec<_>>(),
    );
    let target_doc_ids = engine.table_doc_ids(&target)?;
    let mut locked_targets = Vec::new();
    let mut locked_ids = std::collections::BTreeSet::new();
    for doc_id in target_doc_ids {
        cancel.check()?;
        let Some(candidate) = engine.get_document(&target, doc_id)? else {
            continue;
        };
        let candidate_row =
            dml_target_row(engine, &target, &stmt.target_qualifier, doc_id, &candidate)?;
        let Some(candidate_source) =
            matching_update_source(engine, stmt, ctes, &from_rows, &candidate_row, params)?
        else {
            continue;
        };
        let MutationLockTarget::Present { doc_id, recheck } =
            lock_mutation_target(engine, &target, &stmt.target_qualifier, doc_id, strength)?
        else {
            continue;
        };
        if locked_ids.insert(doc_id) {
            locked_targets.push((doc_id, recheck, candidate_source));
        }
    }
    if locked_targets.iter().any(|(_, recheck, _)| *recheck) {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let mut prepared_updates = Vec::with_capacity(locked_targets.len());
    let mut rewrite_stack = Vec::new();
    for (doc_id, recheck, candidate_source) in locked_targets {
        cancel.check()?;
        let Some(mut doc) = engine.get_document(&target, doc_id)? else {
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
        if recheck
            && !update_source_still_matches(
                engine,
                stmt,
                ctes,
                &target_row,
                &candidate_source,
                params,
            )?
        {
            continue;
        }
        let source_context = candidate_source;
        let joined = dml_join_rows(&target_row, &source_context);
        // Apply assignments evaluated against the rechecked joined row so RHS
        // expressions cannot consume a target image from before the lock wait.
        for assignment in &stmt.assignments {
            let value = eval_mutation_assignment(
                engine,
                ctes,
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
        if let Some(prepared) = prepare_document_rewrite(
            engine,
            &target,
            doc_id,
            original_doc,
            doc,
            params,
            &mut rewrite_stack,
        )? {
            prepared_updates.push((prepared, source_context));
        }
    }
    let prebuild_locking_returning = returning_has_row_locks(&stmt.returning, ctes)?;
    let mut prebuilt_returning_rows = Vec::new();
    if !prepared_updates.is_empty() {
        if prebuild_locking_returning {
            for (prepared, source_context) in &prepared_updates {
                prebuilt_returning_rows.push(prebuild_locking_returning_row(
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
                                doc_id: integer_primary_key_doc_id(
                                    engine,
                                    &prepared.table,
                                    &prepared.new_document,
                                )?
                                .unwrap_or(prepared.doc_id),
                                document: &prepared.new_document,
                            }),
                        },
                        aliases: &stmt.returning_aliases,
                        context: Some(source_context),
                    },
                    &stmt.returning,
                    params,
                    ctes,
                )?);
            }
        }
        engine.prepare_explicit_transaction_writer()?;
    }
    let mut prebuilt_returning_rows = prebuilt_returning_rows.into_iter();
    for (mut prepared, source_context) in prepared_updates {
        let rewritten_doc_id = apply_prepared_document_rewrite(engine, &mut prepared, params)?;
        if !stmt.returning.is_empty() {
            returning_rows.push(if prebuild_locking_returning {
                prebuilt_returning_rows.next().ok_or_else(|| {
                    SQLError::Internal("UPDATE FROM lost a prebuilt RETURNING row".into())
                })?
            } else {
                build_returning_row(
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
                    ctes,
                )?
            });
        }
        affected += 1;
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

fn update_source_still_matches(
    engine: &Engine,
    stmt: &UpdatePlan,
    ctes: &CteScope,
    target_row: &uqa_execution::OwnedPhysicalRow,
    source_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let joined = dml_join_rows(target_row, source_row);
    stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
            .map(|value| uqa_sql::expr::truthy(&value))
    })
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
        let joined = dml_join_rows(target_row, &source_context);
        let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
            eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        if qualifies {
            return Ok(Some(source_context));
        }
    }
    Ok(None)
}
