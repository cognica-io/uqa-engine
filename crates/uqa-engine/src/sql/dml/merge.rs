//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE matching, action execution, and RETURNING projection.

use super::{
    apply_missing_column_defaults, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, decode_merge_pair,
    delete_document_with_referential_actions, dml_join_rows, dml_null_target_row,
    dml_returning_result, dml_storage_error, dml_target_row, doc_id_value, document_supplied_id,
    encode_merge_pair, eval_mutation_assignment, eval_mutation_expr,
    expanded_returning_projections, insert_identity_columns,
    insert_prepared_document_with_constraints, merge_pair_schema, merge_source_index_value,
    missing_document_error, returning_row_context, rewrite_document_with_referential_actions,
    validate_mutation_columns, validate_returning_alias_relations, BTreeSet, CteScope,
    DmlReturningShape, Document, Engine, MergePlan, MergeWhenPlan, MutationAssignmentTarget,
    ProjectionPlan, ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult, Value,
    MERGE_ACTION_COLUMN,
};

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_merge(
    engine: &Engine,
    stmt: MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_merge_inner(engine, &stmt, params))
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_merge_inner(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::truthy;
    let target_table = stmt.target.clone();
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
    let pairing_reader = pairings
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?;
    for pair in pairing_reader {
        let pair = pair.map_err(crate::sql::select::physical_exec_error)?;
        let pair = decode_merge_pair(pair)?;
        // MERGE matched semantics: a target row is "matched" only when
        // the join produced a source pairing. A target row that has
        // no corresponding source counts as unmatched and falls
        // through to the WHEN NOT MATCHED branches.
        let matched = pair.doc_id.is_some();
        let target_document = match pair.doc_id {
            Some(doc_id) => Some(
                engine
                    .get_document(&target_table, doc_id)?
                    .ok_or_else(|| missing_document_error("MERGE action", &target_table, doc_id))?,
            ),
            None => None,
        };
        let target_row = match (pair.doc_id, target_document.as_ref()) {
            (Some(doc_id), Some(document)) => {
                dml_target_row(engine, &target_table, &target_qual, doc_id, document)?
            }
            _ => dml_null_target_row(engine, &target_table, &target_qual)?,
        };
        let joined = dml_join_rows(&target_row, &pair.source_row);
        for clause in &stmt.when_clauses {
            let (condition, action_idx, applies) = match clause {
                MergeWhenPlan::UpdateMatched { condition, .. } if matched => {
                    (condition.as_ref(), 0_u8, true)
                }
                MergeWhenPlan::DeleteMatched { condition } if matched => {
                    (condition.as_ref(), 1, true)
                }
                MergeWhenPlan::NothingMatched { condition } if matched => {
                    (condition.as_ref(), 2, true)
                }
                MergeWhenPlan::InsertNotMatched { condition, .. } if !matched => {
                    (condition.as_ref(), 3, true)
                }
                MergeWhenPlan::NothingNotMatched { condition } if !matched => {
                    (condition.as_ref(), 2, true)
                }
                _ => (None, 0, false),
            };
            if !applies {
                continue;
            }
            if let Some(c) = condition {
                if !truthy(&eval_mutation_expr(
                    engine,
                    &ctes,
                    c,
                    Some(&joined),
                    params,
                )?) {
                    continue;
                }
            }
            match (action_idx, clause) {
                (0, MergeWhenPlan::UpdateMatched { assignments, .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        let mut doc = target_document.clone().ok_or_else(|| {
                            missing_document_error("MERGE matched update", &target_table, doc_id)
                        })?;
                        let original_doc = doc.clone();
                        for assignment in assignments {
                            let value = eval_mutation_assignment(
                                engine,
                                &ctes,
                                MutationAssignmentTarget {
                                    table: &target_table,
                                    column: &assignment.column,
                                    action: "MERGE UPDATE",
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
                        let rewritten_doc_id = rewrite_document_with_referential_actions(
                            engine,
                            &target_table,
                            doc_id,
                            &original_doc,
                            &mut doc,
                            params,
                        )?;
                        affected += 1;
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_merge_returning_row(
                                engine,
                                MergeReturningRow {
                                    target_table: &target_table,
                                    target_qual: &target_qual,
                                    images: ReturningRowImages {
                                        old: Some(ReturningRowImage {
                                            doc_id,
                                            document: &original_doc,
                                        }),
                                        new: Some(ReturningRowImage {
                                            doc_id: rewritten_doc_id,
                                            document: &doc,
                                        }),
                                    },
                                    returning_aliases: &stmt.returning_aliases,
                                    source_row: &pair.source_row,
                                    action: "UPDATE",
                                },
                                &stmt.returning,
                                params,
                                &ctes,
                            )?);
                        }
                    }
                }
                (1, MergeWhenPlan::DeleteMatched { .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        let returning_doc = if stmt.returning.is_empty() {
                            None
                        } else {
                            Some(engine.get_document(&target_table, doc_id)?.ok_or_else(|| {
                                missing_document_error(
                                    "MERGE matched delete",
                                    &target_table,
                                    doc_id,
                                )
                            })?)
                        };
                        let root_deletes = BTreeSet::from([(target_table.clone(), doc_id)]);
                        let mut delete_stack = Vec::new();
                        delete_document_with_referential_actions(
                            engine,
                            &target_table,
                            doc_id,
                            params,
                            &root_deletes,
                            &mut delete_stack,
                        )?;
                        affected += 1;
                        if let Some(doc) = returning_doc.as_ref() {
                            returning_rows.push(build_merge_returning_row(
                                engine,
                                MergeReturningRow {
                                    target_table: &target_table,
                                    target_qual: &target_qual,
                                    images: ReturningRowImages {
                                        old: Some(ReturningRowImage {
                                            doc_id,
                                            document: doc,
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
                            )?);
                        }
                    }
                }
                (
                    3,
                    MergeWhenPlan::InsertNotMatched {
                        columns, values, ..
                    },
                ) => {
                    let mut document = Document::new();
                    let implicit_columns = columns.is_empty();
                    let target_columns = if implicit_columns {
                        engine
                            .try_table_columns(&target_table)
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
                        &target_table,
                        target_columns.iter().map(String::as_str),
                        "MERGE INSERT",
                    )?;
                    for (i, col) in target_columns.iter().take(values.len()).enumerate() {
                        let value = eval_mutation_assignment(
                            engine,
                            &ctes,
                            MutationAssignmentTarget {
                                table: &target_table,
                                column: col,
                                action: "MERGE INSERT",
                            },
                            &values[i],
                            Some(&joined),
                            params,
                        )?;
                        if let Some(value) = value {
                            document.insert(col.clone(), value);
                        }
                    }
                    apply_missing_column_defaults(engine, &target_table, &mut document, params)?;
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
                    engine
                        .advance_next_id(&target_table, doc_id)
                        .map_err(|err| dml_storage_error("MERGE INSERT", err))?;
                    crate::sql::generated::refresh_stored_generated_columns(
                        engine,
                        &target_table,
                        &mut document,
                    )?;
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
                        returning_rows.push(build_merge_returning_row(
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
                        )?);
                    }
                }
                (2, _) => {}
                _ => {}
            }
            break;
        }
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
    let current = input
        .images
        .new
        .or(input.images.old)
        .ok_or_else(|| SQLError::Internal("MERGE RETURNING has no target row image".into()))?;
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
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, ctes).map_err(
        |err| {
            SQLError::Internal(format!(
                "MERGE RETURNING projection failed for table `{}` doc {}: {err}",
                input.target_table, current.doc_id
            ))
        },
    )
}
