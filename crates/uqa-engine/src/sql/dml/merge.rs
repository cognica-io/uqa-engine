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
    doc_id_value, document_supplied_id, document_vectors, encode_merge_pair,
    encode_prepared_doc_id, encode_prepared_document_delete, encode_prepared_document_rewrite,
    eval_mutation_assignment, eval_mutation_expr, expanded_returning_projections,
    insert_identity_columns, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, lock_mutation_target, merge_pair_schema,
    merge_source_index_value, missing_document_error, prepare_document_delete,
    prepare_document_rewrite, returning_row_context, stage_prepared_document_delete,
    stage_prepared_document_rewrite, update_lock_strength, validate_document_constraints,
    validate_mutation_columns, validate_returning_alias_relations, BTreeMap, BTreeSet, CteScope,
    DmlCommandMutationOverlay, DmlReturningShape, Document, Engine, MergePlan, MergeWhenPlan,
    MutationAssignmentTarget, MutationLockTarget, ProjectionPlan, ReturningRowImage,
    ReturningRowImages, SQLError, SQLParam, SQLResult, Value, MERGE_ACTION_COLUMN,
};

const MERGE_PREPARED_UPDATE: i64 = 1;
const MERGE_PREPARED_DELETE: i64 = 2;
const MERGE_PREPARED_INSERT: i64 = 3;
const MERGE_INSERT_DOC_ID: &str = "__uqa_merge_insert_doc_id";
const MERGE_INSERT_DOCUMENT: &str = "__uqa_merge_insert_document";
const MERGE_RETURNING_SOURCE_QUALIFIER: &str = "\0uqa.merge.returning.source";

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
    let source_rows = build_join_spill_with_ctes(engine, &stmt.source, params, &mut ctes)?;
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

    for doc_id in &engine.table_doc_ids(&target_table)? {
        let Some(doc) = engine.get_document(&target_table, *doc_id)? else {
            return Err(missing_document_error("MERGE scan", &target_table, *doc_id));
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
                            Some(*doc_id),
                            Some(&doc),
                            &src,
                        )],
                    ))
                    .map_err(crate::sql::select::physical_exec_error)?;
            }
        }
        if target_matched && matched_can_mutate {
            lock_target_ids.insert(*doc_id);
        } else if !target_matched && has_source_missing_clause {
            if source_missing_can_mutate {
                lock_target_ids.insert(*doc_id);
            }
            pairings
                .push(uqa_execution::Batch::from_physical_rows(
                    pair_schema.clone(),
                    vec![encode_merge_pair(
                        super::MergePairKind::NotMatchedBySource,
                        Some(*doc_id),
                        Some(&doc),
                        &null_source_row,
                    )],
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
                vec![encode_merge_pair(
                    super::MergePairKind::NotMatchedByTarget,
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
    let merge_lock_strength = merge_target_lock_strength(engine, stmt, &target_table);
    let mut recheck_matches = false;
    // A paired target may have been moved to a successor identity by a primary-key rewrite another transaction committed while this statement waited; PostgreSQL 18 follows the update chain, so the pairing is redirected to the successor before the actions run.
    let mut successors: BTreeMap<uqa_core::DocId, uqa_core::DocId> = BTreeMap::new();
    let mut rechecked_target_ids = BTreeSet::new();
    let mut deleted_targets = BTreeSet::new();
    for doc_id in lock_target_ids {
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
                if recheck || locked_id != doc_id {
                    rechecked_target_ids.insert(doc_id);
                }
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
    let mut refreshed_targets = BTreeMap::new();
    for original_id in rechecked_target_ids {
        let locked_id = successors.get(&original_id).copied().unwrap_or(original_id);
        if let Some(document) = engine.get_document(&target_table, locked_id)? {
            refreshed_targets.insert(original_id, (locked_id, document));
        } else {
            deleted_targets.insert(original_id);
        }
    }
    let action_schema = merge_prepared_action_schema();
    let mut prepared_actions = uqa_execution::SpillBuffer::new(work_mem);
    let mut root_deletes = BTreeSet::new();
    let mut has_mutation = false;
    let mut mutated_target_ids = BTreeSet::new();
    let mut rewrite_stack = Vec::new();
    let mut delete_stack = Vec::new();
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let overlay = DmlCommandMutationOverlay::new(engine);
    let pairing_reader = pairings
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for pair in pairing_reader {
        let pair = pair.map_err(crate::sql::select::physical_exec_error)?;
        let mut pair = decode_merge_pair(pair)?;
        let original_doc_id = pair.doc_id;
        if original_doc_id.is_some_and(|doc_id| deleted_targets.contains(&doc_id)) {
            match pair.kind {
                super::MergePairKind::Matched => {
                    pair.kind = super::MergePairKind::NotMatchedByTarget;
                    pair.doc_id = None;
                    pair.target_document = None;
                }
                super::MergePairKind::NotMatchedBySource => continue,
                super::MergePairKind::NotMatchedByTarget => {}
            }
        } else if let Some((successor, document)) =
            original_doc_id.and_then(|doc_id| refreshed_targets.get(&doc_id))
        {
            pair.doc_id = Some(*successor);
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
                new_document,
            } => {
                ensure_merge_target_is_modified_once(&mut mutated_target_ids, doc_id)?;
                let mut prepared = prepare_document_rewrite(
                    engine,
                    &target_table,
                    doc_id,
                    old_document,
                    new_document,
                    params,
                    &mut rewrite_stack,
                )?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "MERGE rewrite dependency tree was cyclic at its root".into(),
                    )
                })?;
                let rewritten_doc_id =
                    stage_prepared_document_rewrite(engine, &mut prepared, params)?;
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
                ensure_merge_target_is_modified_once(&mut mutated_target_ids, doc_id)?;
                root_deletes.insert((target_table.clone(), doc_id));
                let mut prepared = prepare_document_delete(
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
                stage_prepared_document_delete(engine, &mut prepared, params)?;
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
                    .map_err(|error| dml_storage_error("MERGE INSERT", error))?;
                validate_document_constraints(engine, &target_table, &document, params, None)?;
                engine.stage_command_document(&target_table, doc_id, Some(document.clone()))?;
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
                    encode_merge_prepared_insert(doc_id, document),
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
                let (doc_id, document) = decode_merge_prepared_insert(payload)?;
                engine.add_prepared_document_with_vector_values(
                    &target_table,
                    doc_id,
                    document.clone(),
                    document_vectors(engine, &target_table, &document)?,
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
    if !stmt.returning.is_empty() {
        let projections = expanded_merge_returning_projections(
            engine,
            &target_table,
            &target_qual,
            &stmt.returning_aliases,
            source_rows.row_schema(),
            &stmt.returning,
        )?;
        let returning_source_schema = merge_returning_source_schema(source_rows.row_schema());
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
    mutated_target_ids: &mut BTreeSet<uqa_core::DocId>,
    doc_id: uqa_core::DocId,
) -> Result<(), SQLError> {
    if mutated_target_ids.insert(doc_id) {
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
                crate::sql::generated::refresh_stored_generated_columns(
                    engine,
                    target_table,
                    &mut document,
                )?;
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
    row.schema = uqa_execution::RowSchema::append_typed(
        &row.schema,
        &[(
            MERGE_ACTION_COLUMN.into(),
            Some(uqa_sql::ast::ColumnType::Text),
        )],
    );
    row.row = row.row.append_values(vec![Value::Str(input.action.into())]);
    let source_header_width = input
        .source_row
        .schema
        .len()
        .checked_sub(input.source_schema.len())
        .ok_or_else(|| {
            SQLError::Internal("MERGE RETURNING source schema is wider than its pairing".into())
        })?;
    let aliases = input
        .source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                uqa_execution::ColumnIdentity::qualified(
                    MERGE_RETURNING_SOURCE_QUALIFIER,
                    position.to_string(),
                ),
                source_header_width + position,
            )
        })
        .collect::<Vec<_>>();
    let source_schema =
        uqa_execution::RowSchema::with_identity_aliases(&input.source_row.schema, &aliases);
    row = uqa_execution::OwnedPhysicalRow::new(
        uqa_execution::RowSchema::join(&row.schema, &source_schema, std::iter::empty()),
        uqa_execution::PhysicalRow::concat(&row.row, &input.source_row.row),
    );
    Ok(row)
}

fn merge_returning_source_schema(
    source_schema: &uqa_execution::RowSchema,
) -> uqa_execution::RowSchema {
    let aliases = source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                uqa_execution::ColumnIdentity::qualified(
                    MERGE_RETURNING_SOURCE_QUALIFIER,
                    position.to_string(),
                ),
                position,
            )
        })
        .collect::<Vec<_>>();
    uqa_execution::RowSchema::with_identity_aliases(source_schema, &aliases)
}

fn expanded_merge_returning_projections(
    engine: &Engine,
    target_table: &str,
    target_qualifier: &str,
    aliases: &uqa_sql::ast::ReturningAliases,
    source_schema: &uqa_execution::RowSchema,
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
                        .filter(|(_, column)| {
                            crate::sql::select::visible_projection_source_column(column)
                        })
                        .map(|(position, column)| ProjectionPlan {
                            expr: uqa_execution::ScalarExpr::QualifiedColumn {
                                qualifier: MERGE_RETURNING_SOURCE_QUALIFIER.into(),
                                column: position.to_string(),
                            },
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
