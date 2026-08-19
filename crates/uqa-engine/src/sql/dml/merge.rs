//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE matching, action execution, and RETURNING projection.

use super::{
    apply_missing_column_defaults, apply_prepared_document_delete, apply_prepared_document_rewrite,
    build_join_spill_with_ctes, build_projection_physical_row_with_ctes, decode_merge_pair,
    decode_prepared_doc_id, decode_prepared_document_delete, decode_prepared_document_rewrite,
    dml_join_rows, dml_null_target_row, dml_returning_result, dml_storage_error, dml_target_row,
    doc_id_value, document_supplied_id, encode_merge_pair, encode_prepared_doc_id,
    encode_prepared_document_delete, encode_prepared_document_rewrite, eval_mutation_assignment,
    eval_mutation_expr, expanded_returning_projections, insert_identity_columns,
    insert_prepared_document_with_constraints, integer_primary_key_doc_id,
    lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
    lock_mutation_target, merge_pair_schema, merge_source_index_value, missing_document_error,
    prepare_document_delete, prepare_document_rewrite, returning_has_row_locks,
    returning_row_context, update_lock_strength, validate_mutation_columns,
    validate_returning_alias_relations, BTreeMap, BTreeSet, CteScope, DmlReturningShape, Document,
    Engine, MergePlan, MergeWhenPlan, MutationAssignmentTarget, MutationLockTarget,
    PreparedDocumentRewrite, ProjectionPlan, ReturningRowImage, ReturningRowImages, SQLError,
    SQLParam, SQLResult, Value, MERGE_ACTION_COLUMN,
};

const MERGE_PREPARED_NOTHING: i64 = 0;
const MERGE_PREPARED_UPDATE: i64 = 1;
const MERGE_PREPARED_DELETE: i64 = 2;
const MERGE_PREPARED_INSERT: i64 = 3;
const MERGE_INSERT_DOC_ID: &str = "__uqa_merge_insert_doc_id";
const MERGE_INSERT_DOCUMENT: &str = "__uqa_merge_insert_document";

enum SelectedMergeAction {
    Nothing,
    Update {
        doc_id: uqa_core::DocId,
        old_document: Document,
        new_document: Document,
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
    let target_qual = stmt.target_qualifier.clone();
    let mut ctes = CteScope::new();
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. } => validate_mutation_columns(
                engine,
                &target_table,
                assignments
                    .iter()
                    .map(|assignment| assignment.column.as_str()),
                "MERGE UPDATE",
            )?,
            MergeWhenPlan::InsertNotMatched { columns, .. } => validate_mutation_columns(
                engine,
                &target_table,
                columns.iter().map(String::as_str),
                "MERGE INSERT",
            )?,
            _ => {}
        }
    }
    let source_rows = build_join_spill_with_ctes(engine, &stmt.source, params, &mut ctes)?;
    validate_returning_alias_relations(
        &target_qual,
        &stmt.returning_aliases,
        Some(source_rows.row_schema()),
    )?;
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();

    let pair_schema = merge_pair_schema(source_rows.row_schema());
    let work_mem = crate::sql::select::physical_work_mem_bytes(engine)?.max(1);
    let mut pairings = uqa_execution::SpillBuffer::new(work_mem);
    let mut matched_source = uqa_execution::ExactRowSet::new(work_mem);
    let mut matched_target_ids = BTreeSet::new();

    for doc_id in &engine.table_doc_ids(&target_table)? {
        let Some(doc) = engine.get_document(&target_table, *doc_id)? else {
            return Err(missing_document_error("MERGE scan", &target_table, *doc_id));
        };
        let target_row = dml_target_row(engine, &target_table, &target_qual, *doc_id, &doc)?;
        let mut paired_source: Option<(usize, uqa_execution::OwnedPhysicalRow)> = None;
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
            let joined = dml_join_rows(&target_row, &src);
            if truthy(&eval_mutation_expr(
                engine,
                &ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?) {
                paired_source = Some((idx, src));
                if !matched_source
                    .insert_values(std::slice::from_ref(&index_value))
                    .map_err(crate::sql::select::physical_exec_error)?
                {
                    return Err(SQLError::Internal(
                        "MERGE source pairing was concurrently duplicated".into(),
                    ));
                }
                break;
            }
        }
        // Skip target rows that don't pair with any source row --
        // MERGE only emits an action when the join condition holds.
        if let Some((_idx, source_row)) = paired_source {
            matched_target_ids.insert(*doc_id);
            pairings
                .push(uqa_execution::Batch::from_physical_rows(
                    pair_schema.clone(),
                    vec![encode_merge_pair(Some(*doc_id), &source_row)],
                ))
                .map_err(crate::sql::select::physical_exec_error)?;
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
                vec![encode_merge_pair(None, &src)],
            ))
            .map_err(crate::sql::select::physical_exec_error)?;
    }

    let pairings = pairings
        .into_shared(pair_schema)
        .map_err(crate::sql::select::physical_exec_error)?;
    let merge_lock_strength = merge_target_lock_strength(engine, stmt, &target_table);
    let mut recheck_matches = false;
    // A paired target may have been moved to a successor identity by a
    // primary-key rewrite another transaction committed while this statement
    // waited; PostgreSQL 18 follows the update chain, so the pairing is
    // redirected to the successor before the actions run.
    let mut successors: BTreeMap<uqa_core::DocId, uqa_core::DocId> = BTreeMap::new();
    let mut deleted_targets = BTreeSet::new();
    for doc_id in matched_target_ids {
        let target = lock_mutation_target(
            engine,
            &target_table,
            &target_qual,
            doc_id,
            merge_lock_strength,
        )?;
        match target {
            MutationLockTarget::Present {
                doc_id: locked_id,
                recheck,
            } => {
                recheck_matches |= recheck;
                if locked_id != doc_id {
                    successors.insert(doc_id, locked_id);
                }
            }
            MutationLockTarget::Deleted => {
                recheck_matches = true;
                deleted_targets.insert(doc_id);
            }
        }
    }
    if recheck_matches {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let action_schema = merge_prepared_action_schema();
    let mut selected_actions = uqa_execution::SpillBuffer::new(work_mem);
    let mut root_deletes = BTreeSet::new();
    let pairing_reader = pairings
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for pair in pairing_reader {
        let pair = pair.map_err(crate::sql::select::physical_exec_error)?;
        let mut pair = decode_merge_pair(pair)?;
        if pair
            .doc_id
            .is_some_and(|doc_id| deleted_targets.contains(&doc_id))
        {
            pair.doc_id = None;
        }
        if let Some(successor) = pair
            .doc_id
            .and_then(|doc_id| successors.get(&doc_id).copied())
        {
            pair.doc_id = Some(successor);
        }
        // MERGE matched semantics: a target row is "matched" only when
        // the join produced a source pairing. A target row that has
        // no corresponding source counts as unmatched and falls
        // through to the WHEN NOT MATCHED branches. A paired row that no
        // longer exists was deleted by a transaction that committed after
        // this statement's pairing snapshot; PostgreSQL 18 treats it as no
        // longer matched and lets the source row fall through to the WHEN
        // NOT MATCHED actions.
        let mut matched = pair.doc_id.is_some();
        let mut target_document = match pair.doc_id {
            Some(doc_id) => engine.get_document(&target_table, doc_id)?,
            None => None,
        };
        if matched && target_document.is_none() {
            matched = false;
        }
        let mut target_row = match (pair.doc_id, target_document.as_ref()) {
            (Some(doc_id), Some(document)) => {
                dml_target_row(engine, &target_table, &target_qual, doc_id, document)?
            }
            _ => dml_null_target_row(engine, &target_table, &target_qual)?,
        };
        let mut joined = dml_join_rows(&target_row, &pair.source_row);
        if matched
            && recheck_matches
            && !uqa_sql::expr::truthy(&eval_mutation_expr(
                engine,
                &ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?)
        {
            matched = false;
            target_document = None;
            target_row = dml_null_target_row(engine, &target_table, &target_qual)?;
            joined = dml_join_rows(&target_row, &pair.source_row);
        }
        match select_merge_action(
            engine,
            stmt,
            &target_table,
            matched,
            pair.doc_id,
            target_document.as_ref(),
            &joined,
            params,
            &ctes,
        )? {
            SelectedMergeAction::Nothing => push_merge_prepared_action(
                &mut selected_actions,
                &action_schema,
                MERGE_PREPARED_NOTHING,
                Value::Null,
            )?,
            SelectedMergeAction::Update {
                doc_id,
                old_document,
                new_document,
            } => push_merge_prepared_action(
                &mut selected_actions,
                &action_schema,
                MERGE_PREPARED_UPDATE,
                encode_prepared_document_rewrite(PreparedDocumentRewrite {
                    table: target_table.clone(),
                    doc_id,
                    old_document,
                    new_document,
                    actions: Vec::new(),
                }),
            )?,
            SelectedMergeAction::Delete { doc_id } => {
                root_deletes.insert((target_table.clone(), doc_id));
                push_merge_prepared_action(
                    &mut selected_actions,
                    &action_schema,
                    MERGE_PREPARED_DELETE,
                    encode_prepared_doc_id(doc_id),
                )?;
            }
            SelectedMergeAction::Insert { document } => push_merge_prepared_action(
                &mut selected_actions,
                &action_schema,
                MERGE_PREPARED_INSERT,
                Value::Map(document),
            )?,
        }
    }
    let selected_actions = selected_actions
        .into_shared(action_schema.clone())
        .map_err(crate::sql::select::physical_exec_error)?;
    let mut prepared_actions = uqa_execution::SpillBuffer::new(work_mem);
    let selected_reader = selected_actions
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    let mut has_mutation = false;
    let mut rewrite_stack = Vec::new();
    let mut delete_stack = Vec::new();
    for selected in selected_reader {
        let selected = selected.map_err(crate::sql::select::physical_exec_error)?;
        let (action, payload) = decode_merge_prepared_action(selected)?;
        let payload = match action {
            MERGE_PREPARED_NOTHING => Value::Null,
            MERGE_PREPARED_UPDATE => {
                has_mutation = true;
                let seed = decode_prepared_document_rewrite(payload)?;
                let prepared = prepare_document_rewrite(
                    engine,
                    &seed.table,
                    seed.doc_id,
                    seed.old_document,
                    seed.new_document,
                    params,
                    &mut rewrite_stack,
                )?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "MERGE rewrite dependency tree was cyclic at its root".into(),
                    )
                })?;
                encode_prepared_document_rewrite(prepared)
            }
            MERGE_PREPARED_DELETE => {
                has_mutation = true;
                let doc_id = decode_prepared_doc_id(payload, "MERGE delete action")?;
                let prepared = prepare_document_delete(
                    engine,
                    &target_table,
                    doc_id,
                    params,
                    &root_deletes,
                    &mut delete_stack,
                    &mut rewrite_stack,
                )?
                .ok_or_else(|| {
                    SQLError::Internal("MERGE delete dependency tree was cyclic at its root".into())
                })?;
                encode_prepared_document_delete(prepared)
            }
            MERGE_PREPARED_INSERT => {
                has_mutation = true;
                let Value::Map(mut document) = payload else {
                    return Err(SQLError::Internal(
                        "MERGE insert action spill lost its document".into(),
                    ));
                };
                let (auto_id_col, id_column) =
                    insert_identity_columns(engine, &target_table, "MERGE INSERT")?;
                let doc_id = match document_supplied_id(
                    &document,
                    &id_column,
                    auto_id_col.as_deref() == Some(id_column.as_str()),
                )? {
                    Some(doc_id) => doc_id,
                    None => engine.allocate_next_id(&target_table)?,
                };
                if auto_id_col.as_deref() == Some(id_column.as_str()) {
                    document.insert(id_column, doc_id_value(doc_id)?);
                }
                lock_existing_document_foreign_key_dependencies(engine, &target_table, &document)?;
                let _key_locks =
                    lock_document_key_dependencies(engine, &target_table, &document, None)?;
                engine
                    .advance_next_id(&target_table, doc_id)
                    .map_err(|err| dml_storage_error("MERGE INSERT", err))?;
                encode_merge_prepared_insert(doc_id, document)
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "MERGE selected action spill has unknown kind {action}"
                )))
            }
        };
        push_merge_prepared_action(&mut prepared_actions, &action_schema, action, payload)?;
    }
    let prepared_actions = prepared_actions
        .into_shared(action_schema)
        .map_err(crate::sql::select::physical_exec_error)?;
    let prebuild_locking_returning = returning_has_row_locks(&stmt.returning, &ctes)?;
    let mut prebuilt_returning_rows = Vec::new();
    if has_mutation {
        if prebuild_locking_returning {
            let mut pairing_reader = pairings
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            let prepared_reader = prepared_actions
                .read_rows()
                .map_err(crate::sql::select::physical_exec_error)?;
            for prepared in prepared_reader {
                let prepared = prepared.map_err(crate::sql::select::physical_exec_error)?;
                let pair = pairing_reader
                    .next()
                    .ok_or_else(|| {
                        SQLError::Internal("MERGE RETURNING preflight has no source pairing".into())
                    })?
                    .map_err(crate::sql::select::physical_exec_error)?;
                let pair = decode_merge_pair(pair)?;
                let (action, payload) = decode_merge_prepared_action(prepared)?;
                if let Some(row) = prebuild_merge_returning_row(
                    engine,
                    stmt,
                    &target_table,
                    &target_qual,
                    action,
                    payload,
                    &pair.source_row,
                    params,
                    &ctes,
                )? {
                    prebuilt_returning_rows.push(row);
                }
            }
            if pairing_reader.next().is_some() {
                return Err(SQLError::Internal(
                    "MERGE RETURNING preflight source pairing has no prepared action".into(),
                ));
            }
        }
        engine.prepare_explicit_transaction_writer()?;
    }
    let mut prebuilt_returning_rows = prebuilt_returning_rows.into_iter();
    let mut pairing_reader = pairings
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    let prepared_reader = prepared_actions
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for prepared in prepared_reader {
        let prepared = prepared.map_err(crate::sql::select::physical_exec_error)?;
        let pair = pairing_reader
            .next()
            .ok_or_else(|| {
                SQLError::Internal("MERGE prepared action has no source pairing".into())
            })?
            .map_err(crate::sql::select::physical_exec_error)?;
        let pair = decode_merge_pair(pair)?;
        let (action, payload) = decode_merge_prepared_action(prepared)?;
        let mut prebuilt_returning_row =
            if prebuild_locking_returning && action != MERGE_PREPARED_NOTHING {
                Some(prebuilt_returning_rows.next().ok_or_else(|| {
                    SQLError::Internal("MERGE lost a prebuilt RETURNING row".into())
                })?)
            } else {
                None
            };
        match action {
            MERGE_PREPARED_NOTHING => {}
            MERGE_PREPARED_UPDATE => {
                let mut prepared = decode_prepared_document_rewrite(payload)?;
                let rewritten_doc_id =
                    apply_prepared_document_rewrite(engine, &mut prepared, params)?;
                affected += 1;
                if !stmt.returning.is_empty() {
                    returning_rows.push(match prebuilt_returning_row.take() {
                        Some(row) => row,
                        None => build_merge_returning_row(
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
                                action: "UPDATE",
                            },
                            &stmt.returning,
                            params,
                            &ctes,
                        )?,
                    });
                }
            }
            MERGE_PREPARED_DELETE => {
                let mut prepared = decode_prepared_document_delete(payload)?;
                apply_prepared_document_delete(engine, &mut prepared, params)?;
                affected += 1;
                if !stmt.returning.is_empty() {
                    returning_rows.push(match prebuilt_returning_row.take() {
                        Some(row) => row,
                        None => build_merge_returning_row(
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
                                action: "DELETE",
                            },
                            &stmt.returning,
                            params,
                            &ctes,
                        )?,
                    });
                }
            }
            MERGE_PREPARED_INSERT => {
                let (doc_id, document) = decode_merge_prepared_insert(payload)?;
                let inserted = insert_prepared_document_with_constraints(
                    engine,
                    &target_table,
                    doc_id,
                    document,
                    params,
                    false,
                )?;
                affected += 1;
                if !stmt.returning.is_empty() {
                    returning_rows.push(match prebuilt_returning_row.take() {
                        Some(row) => row,
                        None => build_merge_returning_row(
                            engine,
                            MergeReturningRow {
                                target_table: &target_table,
                                target_qual: &target_qual,
                                images: ReturningRowImages {
                                    old: None,
                                    new: Some(ReturningRowImage {
                                        doc_id,
                                        document: &inserted,
                                    }),
                                },
                                returning_aliases: &stmt.returning_aliases,
                                source_row: &pair.source_row,
                                action: "INSERT",
                            },
                            &stmt.returning,
                            params,
                            &ctes,
                        )?,
                    });
                }
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "MERGE prepared action spill has unknown kind {action}"
                )))
            }
        }
    }
    if pairing_reader.next().is_some() {
        return Err(SQLError::Internal(
            "MERGE source pairing has no prepared action".into(),
        ));
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            DmlReturningShape {
                table: &target_table,
                target_qualifier: &target_qual,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: Some(source_rows.row_schema()),
            },
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

#[allow(clippy::too_many_arguments)]
fn select_merge_action(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
    matched: bool,
    doc_id: Option<uqa_core::DocId>,
    target_document: Option<&Document>,
    joined: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SelectedMergeAction, SQLError> {
    for clause in &stmt.when_clauses {
        let (condition, applies) = match clause {
            MergeWhenPlan::UpdateMatched { condition, .. }
            | MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition }
                if matched =>
            {
                (condition.as_ref(), true)
            }
            MergeWhenPlan::InsertNotMatched { condition, .. }
            | MergeWhenPlan::NothingNotMatched { condition }
                if !matched =>
            {
                (condition.as_ref(), true)
            }
            _ => (None, false),
        };
        if !applies {
            continue;
        }
        if let Some(condition) = condition {
            let value = eval_mutation_expr(engine, ctes, condition, Some(joined), params)?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
        }
        return match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. } => {
                let doc_id = doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE matched update lost its target identity".into())
                })?;
                let old_document = target_document.cloned().ok_or_else(|| {
                    missing_document_error("MERGE matched update", target_table, doc_id)
                })?;
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
                        Some(joined),
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
                })
            }
            MergeWhenPlan::DeleteMatched { .. } => Ok(SelectedMergeAction::Delete {
                doc_id: doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE matched delete lost its target identity".into())
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
                        Some(joined),
                        params,
                    )?;
                    if let Some(value) = value {
                        document.insert(column.clone(), value);
                    }
                }
                apply_missing_column_defaults(engine, target_table, &mut document, params)?;
                crate::sql::generated::refresh_stored_generated_columns(
                    engine,
                    target_table,
                    &mut document,
                )?;
                Ok(SelectedMergeAction::Insert { document })
            }
            MergeWhenPlan::NothingMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {
                Ok(SelectedMergeAction::Nothing)
            }
        };
    }
    Ok(SelectedMergeAction::Nothing)
}

fn merge_prepared_action_schema() -> uqa_execution::RowSchema {
    uqa_execution::RowSchema::new(vec![
        "__uqa_merge_action".into(),
        "__uqa_merge_payload".into(),
    ])
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
    let row = row.view();
    let action = match row.value_at(0) {
        Some(Value::Int(action)) => *action,
        _ => {
            return Err(SQLError::Internal(
                "MERGE prepared action spill lost its action kind".into(),
            ))
        }
    };
    let payload = row
        .value_at(1)
        .cloned()
        .ok_or_else(|| SQLError::Internal("MERGE prepared action spill lost its payload".into()))?;
    Ok((action, payload))
}

fn encode_merge_prepared_insert(doc_id: uqa_core::DocId, document: Document) -> Value {
    Value::Map(BTreeMap::from([
        (MERGE_INSERT_DOC_ID.into(), encode_prepared_doc_id(doc_id)),
        (MERGE_INSERT_DOCUMENT.into(), Value::Map(document)),
    ]))
}

fn decode_merge_prepared_insert(value: Value) -> Result<(uqa_core::DocId, Document), SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "MERGE prepared insert payload is not a map".into(),
        ));
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove(MERGE_INSERT_DOC_ID).ok_or_else(|| {
            SQLError::Internal("MERGE prepared insert payload has no document identity".into())
        })?,
        "MERGE prepared insert",
    )?;
    let document = match fields.remove(MERGE_INSERT_DOCUMENT) {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "MERGE prepared insert payload has no document".into(),
            ))
        }
    };
    Ok((doc_id, document))
}

#[allow(clippy::too_many_arguments)]
fn prebuild_merge_returning_row(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
    target_qual: &str,
    action: i64,
    payload: Value,
    source_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<uqa_execution::OwnedPhysicalRow>, SQLError> {
    match action {
        MERGE_PREPARED_NOTHING => Ok(None),
        MERGE_PREPARED_UPDATE => {
            let prepared = decode_prepared_document_rewrite(payload)?;
            let rewritten_doc_id =
                integer_primary_key_doc_id(engine, &prepared.table, &prepared.new_document)?
                    .unwrap_or(prepared.doc_id);
            let row = build_merge_returning_row(
                engine,
                MergeReturningRow {
                    target_table,
                    target_qual,
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
                    source_row,
                    action: "UPDATE",
                },
                &stmt.returning,
                params,
                ctes,
            )?;
            Ok(Some(row))
        }
        MERGE_PREPARED_DELETE => {
            let prepared = decode_prepared_document_delete(payload)?;
            let row = build_merge_returning_row(
                engine,
                MergeReturningRow {
                    target_table,
                    target_qual,
                    images: ReturningRowImages {
                        old: Some(ReturningRowImage {
                            doc_id: prepared.doc_id,
                            document: &prepared.document,
                        }),
                        new: None,
                    },
                    returning_aliases: &stmt.returning_aliases,
                    source_row,
                    action: "DELETE",
                },
                &stmt.returning,
                params,
                ctes,
            )?;
            Ok(Some(row))
        }
        MERGE_PREPARED_INSERT => {
            let (doc_id, document) = decode_merge_prepared_insert(payload)?;
            let row = build_merge_returning_row(
                engine,
                MergeReturningRow {
                    target_table,
                    target_qual,
                    images: ReturningRowImages {
                        old: None,
                        new: Some(ReturningRowImage {
                            doc_id,
                            document: &document,
                        }),
                    },
                    returning_aliases: &stmt.returning_aliases,
                    source_row,
                    action: "INSERT",
                },
                &stmt.returning,
                params,
                ctes,
            )?;
            Ok(Some(row))
        }
        _ => Err(SQLError::Internal(format!(
            "MERGE RETURNING preflight has unknown action kind {action}"
        ))),
    }
}

fn merge_target_lock_strength(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
) -> uqa_sql::ast::LockStrength {
    if stmt
        .when_clauses
        .iter()
        .any(|clause| matches!(clause, MergeWhenPlan::DeleteMatched { .. }))
    {
        return uqa_sql::ast::LockStrength::ForUpdate;
    }
    let columns = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. } => Some(assignments),
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

pub(in crate::sql) struct MergeReturningRow<'a> {
    target_table: &'a str,
    target_qual: &'a str,
    images: ReturningRowImages<'a>,
    returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    source_row: &'a uqa_execution::OwnedPhysicalRow,
    action: &'a str,
}

pub(in crate::sql) fn build_merge_returning_row(
    engine: &Engine,
    input: MergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let mut row = returning_row_context(
        engine,
        input.target_table,
        input.target_qual,
        input.images,
        input.returning_aliases,
    )?;
    row.schema = uqa_execution::RowSchema::append_typed(
        &row.schema,
        &[(
            MERGE_ACTION_COLUMN.into(),
            Some(uqa_sql::ast::ColumnType::Text),
        )],
    );
    row.row = row.row.append_values(vec![Value::Str(input.action.into())]);
    row = uqa_execution::OwnedPhysicalRow::new(
        uqa_execution::RowSchema::join(&row.schema, &input.source_row.schema, std::iter::empty()),
        uqa_execution::PhysicalRow::concat(&row.row, &input.source_row.row),
    );
    let projections = expanded_returning_projections(
        engine,
        input.target_table,
        input.target_qual,
        input.returning_aliases,
        returning,
    )?;
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, ctes)
}
