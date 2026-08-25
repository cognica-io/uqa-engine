//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution and referenced-key delete actions.

use super::{
    apply_set_action_to_child, apply_validated_prepared_document_rewrite,
    build_join_spill_with_ctes, build_returning_row, dml_join_rows, dml_returning_result,
    dml_target_row, eval_mutation_expr, finalize_referential_partition_rewrite,
    foreign_key_comparison_types, foreign_key_lookup_values, lock_mutation_target,
    lock_physical_mutation_target, period_foreign_key_coverage, prepare_document_rewrite,
    referencing_rows, referrers_to_for_actions, stage_prepared_document_rewrite,
    validate_dml_expression_qualifiers, validate_returning_alias_relations, BTreeSet, CteScope,
    DeletePlan, DmlCommandMutationOverlay, DmlReturningShape, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, MutationLockTarget, PhysicalMutationLockTarget, PreparedDeleteAction,
    PreparedDocumentDelete, ReturningProjectionRow, ReturningRowImage, ReturningRowImages,
    SQLError, SQLParam, SQLResult, Value,
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
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut qualified_targets: Vec<(
        String,
        uqa_core::DocId,
        Document,
        Option<uqa_execution::OwnedPhysicalRow>,
    )> = Vec::new();
    let mut returning_rows = Vec::new();
    let mut ctes = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
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
            engine, source, params, &mut ctes,
        )?),
        None => None,
    };
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
    let target_tables = engine.hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
    let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("DELETE preselection is missing its predicate".into())
        })?;
        let mut candidates = Vec::new();
        for table in &target_tables {
            candidates.extend(
                crate::sql::where_eval::collect_where_doc_ids(
                    engine,
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
                engine
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
            let Some(candidate) = qualified_delete_candidate(
                engine,
                stmt,
                &storage_table,
                params,
                &snapshot_ctes,
                using_rows.as_ref(),
                doc_id,
            )?
            else {
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
                recheck_delete_candidate(
                    engine,
                    stmt,
                    &storage_table,
                    params,
                    &snapshot_ctes,
                    doc_id,
                    Some(source_context),
                )?
            } else {
                recheck_delete_candidate(
                    engine,
                    stmt,
                    &storage_table,
                    params,
                    &snapshot_ctes,
                    doc_id,
                    None,
                )?
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
        engine.stage_command_document(&storage_table, doc_id, None)?;
        qualified_targets.push((storage_table, doc_id, doc, returning_context));
    }
    drop(qualification_overlay);
    let to_delete = qualified_targets;
    let root_deletes: BTreeSet<(String, DocId)> = to_delete
        .iter()
        .map(|(table, doc_id, _, _)| (table.clone(), *doc_id))
        .collect();
    let mut delete_stack = Vec::new();
    let mut rewrite_stack = Vec::new();
    let mut prepared_deletes = Vec::with_capacity(to_delete.len());
    let overlay = DmlCommandMutationOverlay::new(engine);
    for (storage_table, doc_id, _doc, returning_context) in to_delete {
        if let Some(mut prepared) = prepare_document_delete(
            engine,
            &storage_table,
            doc_id,
            params,
            &root_deletes,
            &mut delete_stack,
            &mut rewrite_stack,
        )? {
            stage_prepared_document_delete(engine, &mut prepared, params)?;
            affected += 1;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    ReturningProjectionRow {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        images: ReturningRowImages {
                            old: Some(ReturningRowImage {
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
            prepared_deletes.push((prepared, returning_context));
        }
    }
    drop(overlay);
    if !prepared_deletes.is_empty() {
        engine.prepare_explicit_transaction_writer()?;
        for (mut prepared, _) in prepared_deletes {
            apply_validated_prepared_document_delete(engine, &mut prepared)?;
        }
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            DmlReturningShape {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: using_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

fn recheck_delete_candidate(
    engine: &Engine,
    stmt: &DeletePlan,
    storage_table: &str,
    params: &[SQLParam],
    ctes: &CteScope,
    doc_id: DocId,
    source_context: Option<&uqa_execution::OwnedPhysicalRow>,
) -> Result<Option<(Document, Option<uqa_execution::OwnedPhysicalRow>)>, SQLError> {
    let Some(doc) = engine.get_document(storage_table, doc_id)? else {
        return Ok(None);
    };
    let target_row = dml_target_row(engine, &stmt.table, &stmt.target_qualifier, doc_id, &doc)?;
    let joined = source_context
        .map(|source_context| dml_join_rows(&target_row, source_context))
        .unwrap_or(target_row);
    let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
            .map(|value| uqa_sql::expr::truthy(&value))
    })?;
    Ok(qualifies.then(|| (doc, source_context.cloned())))
}

fn qualified_delete_candidate(
    engine: &Engine,
    stmt: &DeletePlan,
    storage_table: &str,
    params: &[SQLParam],
    ctes: &CteScope,
    using_rows: Option<&uqa_execution::SharedSpill>,
    doc_id: DocId,
) -> Result<Option<(Document, Option<uqa_execution::OwnedPhysicalRow>)>, SQLError> {
    let Some(doc) = engine.get_document(storage_table, doc_id)? else {
        return Ok(None);
    };
    let target_row = dml_target_row(engine, &stmt.table, &stmt.target_qualifier, doc_id, &doc)?;
    match using_rows {
        None => {
            let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
                eval_mutation_expr(engine, ctes, filter, Some(&target_row), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            Ok(qualifies.then_some((doc, None)))
        }
        Some(rows) => {
            let reader = rows
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            for using_row in reader {
                let source_context = using_row.map_err(crate::sql::select::physical_exec_error)?;
                let joined = dml_join_rows(&target_row, &source_context);
                let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
                    eval_mutation_expr(engine, ctes, filter, Some(&joined), params)
                        .map(|value| uqa_sql::expr::truthy(&value))
                })?;
                if qualifies {
                    return Ok(Some((doc, Some(source_context))));
                }
            }
            Ok(None)
        }
    }
}

pub(in crate::sql) fn prepare_document_delete(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    delete_stack: &mut Vec<(String, DocId)>,
    rewrite_stack: &mut Vec<(String, DocId)>,
) -> Result<Option<PreparedDocumentDelete>, SQLError> {
    let key = (table.to_string(), doc_id);
    if delete_stack.contains(&key) {
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
    let Some(target) = engine.get_document(table, doc_id)? else {
        return Ok(None);
    };
    delete_stack.push((table.to_string(), doc_id));
    let actions = prepare_referenced_key_delete_actions(
        engine,
        ReferencedDelete {
            table,
            doc_id,
            document: &target,
        },
        params,
        root_deletes,
        delete_stack,
        rewrite_stack,
    );
    delete_stack.pop();
    Ok(Some(PreparedDocumentDelete {
        table: table.to_string(),
        doc_id,
        document: target,
        actions: actions?,
    }))
}

pub(in crate::sql) fn stage_prepared_document_delete(
    engine: &Engine,
    prepared: &mut PreparedDocumentDelete,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for action in &mut prepared.actions {
        match action {
            PreparedDeleteAction::Delete(delete) => {
                stage_prepared_document_delete(engine, delete, params)?;
            }
            PreparedDeleteAction::Rewrite(rewrite) => {
                stage_prepared_document_rewrite(engine, rewrite, params)?;
            }
        }
    }
    engine.stage_command_document(&prepared.table, prepared.doc_id, None)
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
    delete_stack: &mut Vec<(String, DocId)>,
    rewrite_stack: &mut Vec<(String, DocId)>,
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
                    if fk.deferrable && fk.initially_deferred {
                        engine.defer_foreign_key_row(&physical_table, child_id)?;
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
        let referencing = referencing_rows(engine, &ref_table, &fk, &comparison, &expected)?;
        for (child, _child_doc) in referencing {
            if root_deletes.contains(&(child.table.clone(), child.doc_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction if fk.deferrable && fk.initially_deferred => {
                    engine.defer_foreign_key_row(&child.table, child.doc_id)?;
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
                        delete_stack,
                        rewrite_stack,
                    )? {
                        actions.push(PreparedDeleteAction::Delete(Box::new(prepared)));
                    }
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let columns = delete_set_columns(&fk);
                    let Some((child, child_doc)) = super::lock_referencing_child(
                        engine,
                        &ref_table,
                        &child,
                        &columns,
                        &fk,
                        &comparison,
                        &expected,
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
                    if let Some(mut prepared) = prepare_document_rewrite(
                        engine,
                        &child.table,
                        child.doc_id,
                        child_doc,
                        updated,
                        params,
                        rewrite_stack,
                    )? {
                        finalize_referential_partition_rewrite(engine, &mut prepared, params)?;
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
