//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{
    build_join_spill_with_ctes, build_projection_row_with_ctes, coerce_to_column_type,
    column_type_name, doc_id_value, expand_star_columns, prefix_row, projection_columns,
    validate_vector_dimensions, value_to_tensor, value_to_vector, BTreeMap, BTreeSet, BinaryOp,
    ColumnType, CteScope, DocId, Document, Engine, ForeignKey, ForeignKeyAction, ForeignKeyMatch,
    ResultRow, RowIndependentUpdateValues, SQLError, SQLParam, SQLResult, Value, DOC_ID_COLUMN,
    MERGE_ACTION_COLUMN,
};
use uqa_execution::ScalarExpr;
use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, SourcePlan, UpdatePlan,
};

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String), SQLError> {
    let auto_increment = engine
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let id_column = if auto_increment.is_some() {
        auto_increment.clone()
    } else {
        engine
            .try_describe_table(table)
            .map_err(|err| dml_storage_error(action, err))?
            .and_then(|columns| columns.into_iter().find(|column| column.primary_key))
            .map(|column| column.name)
    }
    .unwrap_or_else(|| "id".into());
    Ok((auto_increment, id_column))
}

fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    Ok(())
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    eval_physical_scalar(expression, &ctes.scalar_subqueries, &context)
}

const MERGE_PAIR_DOC_ID: &str = "__uqa_merge_pair_doc_id";

struct MergePairing {
    doc_id: Option<DocId>,
    target_row: ResultRow,
    source_row: Option<ResultRow>,
}

fn merge_pair_schema(target_columns: &[String], source_columns: &[String]) -> Vec<String> {
    std::iter::once(MERGE_PAIR_DOC_ID.to_string())
        .chain(
            target_columns
                .iter()
                .enumerate()
                .map(|(index, _)| format!("__uqa_merge_target_{index}")),
        )
        .chain(
            source_columns
                .iter()
                .enumerate()
                .map(|(index, _)| format!("__uqa_merge_source_{index}")),
        )
        .collect()
}

fn encode_merge_pair(
    doc_id: Option<DocId>,
    target_row: &ResultRow,
    source_row: &ResultRow,
    target_columns: &[String],
    source_columns: &[String],
) -> ResultRow {
    let mut encoded = ResultRow::new();
    encoded.insert(
        MERGE_PAIR_DOC_ID.into(),
        doc_id.map_or(Value::Null, |doc_id| Value::Str(doc_id.to_string())),
    );
    for (index, column) in target_columns.iter().enumerate() {
        encoded.insert(
            format!("__uqa_merge_target_{index}"),
            target_row.get(column).cloned().unwrap_or(Value::Null),
        );
    }
    for (index, column) in source_columns.iter().enumerate() {
        encoded.insert(
            format!("__uqa_merge_source_{index}"),
            source_row.get(column).cloned().unwrap_or(Value::Null),
        );
    }
    encoded
}

fn decode_merge_pair(
    encoded: &ResultRow,
    target_columns: &[String],
    source_columns: &[String],
) -> Result<MergePairing, SQLError> {
    let doc_id = match encoded.get(MERGE_PAIR_DOC_ID) {
        Some(Value::Null) | None => None,
        Some(Value::Str(doc_id)) => Some(doc_id.parse::<DocId>().map_err(|error| {
            SQLError::Internal(format!(
                "invalid spilled MERGE document id `{doc_id}`: {error}"
            ))
        })?),
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE document id value {value:?}"
            )))
        }
    };
    let target_row = target_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            (
                column.clone(),
                encoded
                    .get(&format!("__uqa_merge_target_{index}"))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    let source_row = source_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            (
                column.clone(),
                encoded
                    .get(&format!("__uqa_merge_source_{index}"))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    Ok(MergePairing {
        doc_id,
        target_row,
        source_row: Some(source_row),
    })
}

fn merge_source_index_row(index: usize) -> ResultRow {
    ResultRow::from([("source_index".into(), Value::Str(index.to_string()))])
}

#[allow(clippy::too_many_lines)]
pub(super) fn run_merge(
    engine: &Engine,
    stmt: MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_merge_inner(engine, &stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_merge_inner(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::truthy;
    let target_table = stmt.target.clone();
    let target_qual = stmt
        .target_alias
        .clone()
        .unwrap_or_else(|| target_table.clone());
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
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();

    let target_columns = engine
        .try_table_columns(&target_table)
        .map_err(|error| SQLError::Internal(format!("read MERGE target schema: {error}")))?
        .into_iter()
        .map(|column| format!("{target_qual}.{column}"))
        .collect::<Vec<_>>();
    let source_columns = source_rows.schema().to_vec();
    let pair_schema = merge_pair_schema(&target_columns, &source_columns);
    let pair_row_schema = uqa_execution::RowSchema::new(pair_schema.clone());
    let work_mem = super::select::physical_work_mem_bytes(engine)?.max(1);
    let mut pairings = uqa_execution::SpillBuffer::new(work_mem);
    let source_index_schema = vec!["source_index".to_string()];
    let mut matched_source = uqa_execution::ExactRowSet::new(work_mem);

    for doc_id in &engine.table_doc_ids(&target_table)? {
        let Some(doc) = engine.get_document(&target_table, *doc_id)? else {
            return Err(missing_document_error("MERGE scan", &target_table, *doc_id));
        };
        let target_row = prefix_row(&target_qual, &doc);
        let mut paired_source: Option<(usize, ResultRow)> = None;
        let source_reader = source_rows
            .read_rows()
            .map_err(super::select::physical_exec_error)?;
        for (idx, src) in source_reader.enumerate() {
            let src = src.map_err(super::select::physical_exec_error)?;
            let index_row = merge_source_index_row(idx);
            if matched_source
                .contains_row(&index_row, &source_index_schema)
                .map_err(super::select::physical_exec_error)?
            {
                continue;
            }
            let mut joined = ResultRow::new();
            for (k, v) in &target_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in &src {
                joined.insert(k.clone(), v.clone());
            }
            if truthy(&eval_mutation_expr(
                engine,
                &ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?) {
                paired_source = Some((idx, src));
                if !matched_source
                    .insert_row(&index_row, &source_index_schema)
                    .map_err(super::select::physical_exec_error)?
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
                .push(uqa_execution::Batch::new(
                    pair_row_schema.clone(),
                    vec![encode_merge_pair(
                        Some(*doc_id),
                        &target_row,
                        &source_row,
                        &target_columns,
                        &source_columns,
                    )],
                ))
                .map_err(super::select::physical_exec_error)?;
        }
    }
    let source_reader = source_rows
        .read_rows()
        .map_err(super::select::physical_exec_error)?;
    for (idx, src) in source_reader.enumerate() {
        let src = src.map_err(super::select::physical_exec_error)?;
        let index_row = merge_source_index_row(idx);
        if matched_source
            .contains_row(&index_row, &source_index_schema)
            .map_err(super::select::physical_exec_error)?
        {
            continue;
        }
        pairings
            .push(uqa_execution::Batch::new(
                pair_row_schema.clone(),
                vec![encode_merge_pair(
                    None,
                    &ResultRow::new(),
                    &src,
                    &target_columns,
                    &source_columns,
                )],
            ))
            .map_err(super::select::physical_exec_error)?;
    }

    let pairings = pairings
        .into_shared(pair_schema)
        .map_err(super::select::physical_exec_error)?;
    let pairing_reader = pairings
        .read_rows()
        .map_err(super::select::physical_exec_error)?;
    for pair in pairing_reader {
        let pair = decode_merge_pair(
            &pair.map_err(super::select::physical_exec_error)?,
            &target_columns,
            &source_columns,
        )?;
        // MERGE matched semantics: a target row is "matched" only when
        // the join produced a source pairing. A target row that has
        // no corresponding source counts as unmatched and falls
        // through to the WHEN NOT MATCHED branches.
        let matched = pair.doc_id.is_some() && pair.source_row.is_some();
        let mut joined = pair.target_row.clone();
        if let Some(src) = &pair.source_row {
            for (k, v) in src {
                joined.insert(k.clone(), v.clone());
            }
        }
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
                        let Some(mut doc) = engine.get_document(&target_table, doc_id)? else {
                            return Err(missing_document_error(
                                "MERGE matched update",
                                &target_table,
                                doc_id,
                            ));
                        };
                        let original_doc = doc.clone();
                        for assignment in assignments {
                            let value = coerce_to_column_type(
                                engine,
                                &target_table,
                                &assignment.column,
                                eval_mutation_expr(
                                    engine,
                                    &ctes,
                                    &assignment.value,
                                    Some(&joined),
                                    params,
                                )?,
                            )?;
                            doc.insert(assignment.column.clone(), value);
                        }
                        let rewritten_doc_id = rewrite_document_with_referential_actions(
                            engine,
                            &target_table,
                            doc_id,
                            &original_doc,
                            doc.clone(),
                            params,
                        )?;
                        affected += 1;
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_merge_returning_row(
                                engine,
                                MergeReturningRow {
                                    target_table: &target_table,
                                    target_qual: &target_qual,
                                    doc_id: rewritten_doc_id,
                                    document: &doc,
                                    source_row: pair.source_row.as_ref(),
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
                                    doc_id,
                                    document: doc,
                                    source_row: pair.source_row.as_ref(),
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
                    if values.len() != columns.len() {
                        return Err(SQLError::Internal(format!(
                            "MERGE INSERT row width {} != column count {}",
                            values.len(),
                            columns.len()
                        )));
                    }
                    for (i, col) in columns.iter().enumerate() {
                        let v = coerce_to_column_type(
                            engine,
                            &target_table,
                            col,
                            eval_mutation_expr(engine, &ctes, &values[i], Some(&joined), params)?,
                        )?;
                        document.insert(col.clone(), v);
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
                    let inserted = insert_document_with_constraints(
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
                                doc_id,
                                document: &inserted,
                                source_row: pair.source_row.as_ref(),
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
            &target_table,
            &stmt.returning,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

struct MergeReturningRow<'a> {
    target_table: &'a str,
    target_qual: &'a str,
    doc_id: DocId,
    document: &'a Document,
    source_row: Option<&'a ResultRow>,
    action: &'a str,
}

fn build_merge_returning_row(
    engine: &Engine,
    input: MergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    let mut row_doc = input.document.clone();
    row_doc.insert(DOC_ID_COLUMN.into(), doc_id_value(input.doc_id)?);
    row_doc.insert(MERGE_ACTION_COLUMN.into(), Value::Str(input.action.into()));
    for (key, value) in prefix_row(input.target_qual, input.document) {
        row_doc.insert(key, value);
    }
    if let Some(source) = input.source_row {
        for (key, value) in source {
            row_doc.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    build_projection_row_with_ctes(engine, &row_doc, returning, params, ctes).map_err(|err| {
        SQLError::Internal(format!(
            "MERGE RETURNING projection failed for table `{}` doc {}: {err}",
            input.target_table, input.doc_id
        ))
    })
}

pub(super) fn run_update(
    engine: &Engine,
    stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_update_inner(engine, &stmt, params))
}

fn run_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    validate_mutation_columns(
        engine,
        &stmt.table,
        stmt.assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
        "UPDATE",
    )?;
    let mut ctes = CteScope::new();
    super::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);

    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate the WHERE against each joined row, and apply
    // assignments to the matching target rows. Mirrors the canonical UQA implementation's
    // _compile_update_from.
    if let Some(source) = stmt.source.as_deref() {
        return run_update_from(engine, stmt, source, params, &mut ctes);
    }
    let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
    if !has_runtime_scope {
        if let Some(result) = try_run_point_update(engine, stmt, params)? {
            return Ok(result);
        }
    }
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    // Without CTEs the WHERE clause resolves through the accelerated
    // single-table machinery (value indexes, posting lists) up front;
    // the per-row re-check below is then unnecessary.
    let preselected = !has_runtime_scope && stmt.predicate.is_some();
    let doc_ids: Vec<uqa_core::DocId> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("UPDATE preselection is missing its predicate".into())
        })?;
        super::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
    } else {
        engine.table_doc_ids(&stmt.table)?
    };
    for doc_id in doc_ids {
        cancel.check()?;
        let Some(mut doc) = engine.get_document(&stmt.table, doc_id)? else {
            return Err(missing_document_error("UPDATE scan", &stmt.table, doc_id));
        };
        let original_doc = doc.clone();
        if !preselected {
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    engine,
                    &ctes,
                    filter,
                    Some(&doc),
                    params,
                )?) {
                    continue;
                }
            }
        }
        for assignment in &stmt.assignments {
            let value = coerce_to_column_type(
                engine,
                &stmt.table,
                &assignment.column,
                eval_mutation_expr(engine, &ctes, &assignment.value, Some(&doc), params)?,
            )?;
            doc.insert(assignment.column.clone(), value);
        }
        let rewritten_doc_id = rewrite_document_with_referential_actions(
            engine,
            &stmt.table,
            doc_id,
            &original_doc,
            doc.clone(),
            params,
        )?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                rewritten_doc_id,
                &doc,
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

fn try_run_point_update(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
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
    let affected =
        engine.patch_document_fields_with_vector_values(&stmt.table, doc_id, &updates, &vectors)?;
    Ok(Some(SQLResult::from_affected(u64::from(affected))))
}

fn point_lookup_filter(
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
            let ctes = CteScope::new();
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, rhs, None, params)?,
            )));
        }
    }
    if let Some(field) = top_level_column(rhs) {
        if expr_is_row_independent(lhs) {
            let ctes = CteScope::new();
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, lhs, None, params)?,
            )));
        }
    }
    Ok(None)
}

fn top_level_column(expr: &ScalarExpr) -> Option<&str> {
    match expr {
        ScalarExpr::Column(name) => Some(name),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

fn row_independent_update_values(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<RowIndependentUpdateValues>, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctes = CteScope::new();
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

fn expr_is_row_independent(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().all(expr_is_row_independent)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_is_row_independent(lhs) && expr_is_row_independent(rhs)
        }
        ScalarExpr::Not(inner) => expr_is_row_independent(inner),
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
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Func { .. }
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

fn can_patch_update_without_full_row(
    engine: &Engine,
    table: &str,
    updates: &BTreeMap<String, Value>,
) -> Result<bool, SQLError> {
    if !engine
        .try_check_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .is_empty()
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
                && !col.auto_increment
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

fn point_lookup_field_is_unique(
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

fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints(engine, table, document, params)?;
    validate_key_constraints(engine, table, document, ignored_doc_id)
}

fn validate_document_non_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for col_def in engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
    {
        if !col_def.not_null || col_def.auto_increment {
            continue;
        }
        match document.get(&col_def.name) {
            Some(Value::Null) | None => {
                return Err(SQLError::TypeMismatch(format!(
                    "NOT NULL constraint violated: column `{}` in table `{table}`",
                    col_def.name
                )));
            }
            _ => {}
        }
    }

    for (cname, expr) in engine
        .try_check_constraints(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let result = eval_lowered_expression(engine, &expr, Some(document), params)?;
        if !uqa_sql::expr::truthy(&result) {
            let label = cname.unwrap_or_else(|| "<unnamed>".into());
            return Err(SQLError::TypeMismatch(format!(
                "CHECK constraint `{label}` violated in table `{table}`"
            )));
        }
    }

    for fk in engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(local_values) = foreign_key_lookup_values(&fk, document)? else {
            continue;
        };
        if engine
            .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)?
            .is_none()
        {
            let cols = fk.local_columns.join(", ");
            return Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY constraint violated: ({cols}) -> {}({}) has no matching row",
                fk.ref_table,
                fk.ref_columns.join(", ")
            )));
        }
    }

    Ok(())
}

fn key_constraint_values(
    constraint: &uqa_sql::ast::TableKeyConstraint,
    document: &Document,
) -> Option<Vec<Value>> {
    let values: Vec<Value> = constraint
        .columns
        .iter()
        .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
        && !constraint.nulls_not_distinct
        && values.iter().any(|value| matches!(value, Value::Null))
    {
        return None;
    }
    Some(values)
}

fn validate_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    for constraint in engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(values) = key_constraint_values(&constraint, document) else {
            continue;
        };
        let Some(conflict_id) = engine.find_conflict(table, &constraint.columns, &values)? else {
            continue;
        };
        if ignored_doc_id == Some(conflict_id) {
            continue;
        }
        let kind = match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
            uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
        };
        let name = constraint
            .name
            .as_deref()
            .map_or_else(String::new, |name| format!(" `{name}`"));
        return Err(SQLError::TypeMismatch(format!(
            "{kind} constraint{name} violated: duplicate value for columns ({}) in table `{table}`",
            constraint.columns.join(", ")
        )));
    }
    Ok(())
}

fn foreign_key_lookup_values(
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<Vec<Value>>, SQLError> {
    let local_values: Vec<Value> = fk
        .local_columns
        .iter()
        .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    let null_count = local_values
        .iter()
        .filter(|value| matches!(value, Value::Null))
        .count();
    if null_count == 0 {
        return Ok(Some(local_values));
    }
    match fk.match_type {
        ForeignKeyMatch::Simple => Ok(None),
        ForeignKeyMatch::Full if null_count == local_values.len() => Ok(None),
        ForeignKeyMatch::Full => {
            let cols = fk.local_columns.join(", ");
            Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY MATCH FULL constraint violated: ({cols}) must be all NULL or all non-NULL"
            )))
        }
    }
}

fn rewrite_document_with_referential_actions(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    old_doc: &Document,
    new_doc: Document,
    params: &[SQLParam],
) -> Result<DocId, SQLError> {
    validate_document_constraints(engine, table, &new_doc, params, Some(doc_id))?;
    let rewritten_doc_id = match integer_primary_key_doc_id(engine, table, &new_doc)? {
        // An integer primary key names the row's doc_id slot; keep that
        // invariant when the key itself changes, or value -> doc_id
        // lookups (the unique fast path and FOREIGN KEY validation) read
        // the stale slot and miss the row.
        Some(new_id) if new_id != doc_id => {
            engine.delete_document(table, doc_id)?;
            engine.add_document_with_vector_values(
                table,
                new_id,
                new_doc.clone(),
                document_vectors(engine, table, &new_doc)?,
            )?;
            engine
                .advance_next_id(table, new_id)
                .map_err(|err| dml_storage_error("UPDATE primary key", err))?;
            new_id
        }
        _ => {
            engine.rewrite_document(table, doc_id, new_doc.clone())?;
            doc_id
        }
    };
    apply_referenced_key_update_actions(engine, table, old_doc, &new_doc, params)?;
    Ok(rewritten_doc_id)
}

fn integer_primary_key_doc_id(
    engine: &Engine,
    table: &str,
    doc: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(cols) = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE primary key", err))?
    else {
        return Ok(None);
    };
    let Some(pk) = cols
        .iter()
        .find(|c| c.primary_key && matches!(c.ty, uqa_sql::ast::ColumnType::Integer))
    else {
        return Ok(None);
    };
    Ok(match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    })
}

fn apply_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(engine, table)? {
        let old_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| old_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let new_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| new_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if old_values == new_values || old_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &old_values)?;
        for (child_id, child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: UPDATE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let mut updated = child_doc.clone();
                    for (col, value) in fk.local_columns.iter().zip(new_values.iter()) {
                        updated.insert(col.clone(), value.clone());
                    }
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let mut updated = child_doc.clone();
                    apply_set_action_to_child(
                        engine,
                        &ref_table,
                        &child_doc,
                        &mut updated,
                        &fk.local_columns,
                        fk.on_update,
                        params,
                    )?;
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn referrers_to_for_actions(
    engine: &Engine,
    table: &str,
) -> Result<Vec<(String, ForeignKey)>, SQLError> {
    engine
        .try_referrers_to(table)
        .map_err(|err| dml_storage_error("foreign-key lookup", err))
}

fn referencing_rows(
    engine: &Engine,
    table: &str,
    local_columns: &[String],
    key_values: &[Value],
) -> Result<Vec<(DocId, Document)>, SQLError> {
    if local_columns.is_empty() || local_columns.len() != key_values.len() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            return Err(missing_document_error(
                "foreign-key reference scan",
                table,
                doc_id,
            ));
        };
        let matches = local_columns
            .iter()
            .zip(key_values.iter())
            .all(|(col, want)| doc.get(col).cloned().unwrap_or(Value::Null) == *want);
        if matches {
            out.push((doc_id, doc));
        }
    }
    Ok(out)
}

fn apply_set_action_to_child(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &mut Document,
    columns: &[String],
    action: ForeignKeyAction,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for column in columns {
        let value = match action {
            ForeignKeyAction::SetNull => Value::Null,
            ForeignKeyAction::SetDefault => {
                if let Some(expr) = engine
                    .try_column_default_expr(table, column)
                    .map_err(|err| dml_storage_error("referential SET DEFAULT", err))?
                {
                    eval_lowered_expression(engine, &expr, Some(old_doc), params)?
                } else {
                    Value::Null
                }
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict | ForeignKeyAction::Cascade => {
                return Err(SQLError::Internal(format!(
                    "invalid SET action helper for `{action:?}`"
                )));
            }
        };
        let value = coerce_to_column_type(engine, table, column, value)?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}

fn run_update_from(
    engine: &Engine,
    stmt: &UpdatePlan,
    from_clause: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    let from_rows = build_join_spill_with_ctes(engine, from_clause, params, ctes)?;
    let cancel = engine.cancellation_token();
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let target = stmt.table.clone();
    let target_doc_ids = engine.table_doc_ids(&target)?;
    for doc_id in target_doc_ids {
        cancel.check()?;
        let Some(mut doc) = engine.get_document(&target, doc_id)? else {
            return Err(missing_document_error("UPDATE FROM scan", &target, doc_id));
        };
        let original_doc = doc.clone();
        let mut applied = false;
        let from_reader = from_rows
            .read_rows()
            .map_err(super::select::physical_exec_error)?;
        for from_row in from_reader {
            let from_row = from_row.map_err(super::select::physical_exec_error)?;
            // Build a joined row: target columns are exposed both
            // unqualified and prefixed (`<table>.<col>`) so the
            // WHERE / RHS expressions can use either spelling.
            // FROM-side rows already carry their alias prefix when
            // one was supplied.
            let mut joined = ResultRow::new();
            for (k, v) in &doc {
                joined.insert(k.clone(), v.clone());
                joined.insert(format!("{target}.{k}"), v.clone());
            }
            for (k, v) in &from_row {
                joined.insert(k.clone(), v.clone());
            }
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    engine,
                    ctes,
                    filter,
                    Some(&joined),
                    params,
                )?) {
                    continue;
                }
            }
            // Apply assignments evaluated against the joined row so
            // RHS expressions can read FROM-side columns.
            for assignment in &stmt.assignments {
                let value = coerce_to_column_type(
                    engine,
                    &target,
                    &assignment.column,
                    eval_mutation_expr(engine, ctes, &assignment.value, Some(&joined), params)?,
                )?;
                doc.insert(assignment.column.clone(), value);
            }
            let rewritten_doc_id = rewrite_document_with_referential_actions(
                engine,
                &target,
                doc_id,
                &original_doc,
                doc.clone(),
                params,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &target,
                    rewritten_doc_id,
                    &doc,
                    &stmt.returning,
                    params,
                    ctes,
                )?);
            }
            applied = true;
            break;
        }
        if applied {
            affected += 1;
        }
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(engine, &target, &stmt.returning, returning_rows, affected);
    }
    Ok(SQLResult::from_affected(affected))
}

pub(super) fn run_delete(
    engine: &Engine,
    stmt: DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_delete_inner(engine, &stmt, params))
}

fn run_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut to_delete: Vec<uqa_core::DocId> = Vec::new();
    let mut returning_docs: Vec<(uqa_core::DocId, Document)> = Vec::new();
    let mut ctes = CteScope::new();
    super::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    // DELETE FROM t USING other WHERE ... -- materialise the join
    // first, then collect target doc ids whose joined image
    // satisfies WHERE. Mirrors the canonical UQA implementation's _compile_delete_using.
    let using_rows: Option<uqa_execution::SharedSpill> = match stmt.source.as_deref() {
        Some(source) => Some(build_join_spill_with_ctes(
            engine, source, params, &mut ctes,
        )?),
        None => None,
    };
    let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
    // Plain `DELETE FROM t WHERE ...` resolves the WHERE through the
    // accelerated single-table machinery instead of materialising the
    // whole table.
    let preselected = !has_runtime_scope && stmt.source.is_none() && stmt.predicate.is_some();
    let doc_ids: Vec<uqa_core::DocId> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("DELETE preselection is missing its predicate".into())
        })?;
        super::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
    } else {
        engine.table_doc_ids(&stmt.table)?
    };
    for doc_id in doc_ids {
        cancel.check()?;
        if preselected && stmt.returning.is_empty() {
            // No RETURNING and the filter already matched: the
            // document body is not needed at all.
            to_delete.push(doc_id);
            continue;
        }
        let Some(doc) = engine.get_document(&stmt.table, doc_id)? else {
            return Err(missing_document_error("DELETE scan", &stmt.table, doc_id));
        };
        let keep = match (stmt.predicate.as_ref(), using_rows.as_ref()) {
            (None, _) => true,
            (Some(_), None) if preselected => true,
            (Some(filter), None) => uqa_sql::expr::truthy(&eval_mutation_expr(
                engine,
                &ctes,
                filter,
                Some(&doc),
                params,
            )?),
            (Some(filter), Some(rows)) => {
                let mut matched = false;
                let reader = rows
                    .read_rows()
                    .map_err(super::select::physical_exec_error)?;
                for using_row in reader {
                    let using_row = using_row.map_err(super::select::physical_exec_error)?;
                    let mut joined = ResultRow::new();
                    for (k, v) in &doc {
                        joined.insert(k.clone(), v.clone());
                        joined.insert(format!("{}.{k}", stmt.table), v.clone());
                    }
                    for (k, v) in &using_row {
                        joined.insert(k.clone(), v.clone());
                    }
                    if uqa_sql::expr::truthy(&eval_mutation_expr(
                        engine,
                        &ctes,
                        filter,
                        Some(&joined),
                        params,
                    )?) {
                        matched = true;
                        break;
                    }
                }
                matched
            }
        };
        if keep {
            if !stmt.returning.is_empty() {
                returning_docs.push((doc_id, doc.clone()));
            }
            to_delete.push(doc_id);
        }
    }
    let root_deletes: BTreeSet<(String, DocId)> = to_delete
        .iter()
        .map(|doc_id| (stmt.table.clone(), *doc_id))
        .collect();
    let mut delete_stack = Vec::new();
    for doc_id in to_delete {
        delete_document_with_referential_actions(
            engine,
            &stmt.table,
            doc_id,
            params,
            &root_deletes,
            &mut delete_stack,
        )?;
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        let returning_rows = returning_docs
            .into_iter()
            .map(|(doc_id, doc)| {
                build_returning_row(
                    engine,
                    &stmt.table,
                    doc_id,
                    &doc,
                    &stmt.returning,
                    params,
                    &ctes,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        return dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

fn delete_document_with_referential_actions(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    delete_stack: &mut Vec<(String, DocId)>,
) -> Result<(), SQLError> {
    let key = (table.to_string(), doc_id);
    if delete_stack.contains(&key) {
        return Ok(());
    }
    let Some(target) = engine.get_document(table, doc_id)? else {
        return Ok(());
    };
    delete_stack.push(key);
    apply_referenced_key_delete_actions(
        engine,
        table,
        &target,
        params,
        root_deletes,
        delete_stack,
    )?;
    delete_stack.pop();
    engine.delete_document(table, doc_id)?;
    Ok(())
}

fn apply_referenced_key_delete_actions(
    engine: &Engine,
    table: &str,
    target: &Document,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    delete_stack: &mut Vec<(String, DocId)>,
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(engine, table)? {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| target.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &key_values)?;
        for (child_id, child_doc) in referencing {
            if root_deletes.contains(&(ref_table.clone(), child_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: DELETE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    delete_document_with_referential_actions(
                        engine,
                        &ref_table,
                        child_id,
                        params,
                        root_deletes,
                        delete_stack,
                    )?;
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let mut updated = child_doc.clone();
                    let columns = delete_set_columns(&fk);
                    apply_set_action_to_child(
                        engine,
                        &ref_table,
                        &child_doc,
                        &mut updated,
                        &columns,
                        fk.on_delete,
                        params,
                    )?;
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn delete_set_columns(fk: &ForeignKey) -> Vec<String> {
    if fk.on_delete_set_columns.is_empty() {
        fk.local_columns.clone()
    } else {
        fk.on_delete_set_columns.clone()
    }
}

fn find_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
) -> Result<Option<DocId>, SQLError> {
    let constraints = engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("INSERT conflict lookup", err))?;
    if !on_conflict.conflict_columns.is_empty() {
        let target: BTreeSet<&str> = on_conflict
            .conflict_columns
            .iter()
            .map(String::as_str)
            .collect();
        if target.len() != on_conflict.conflict_columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "ON CONFLICT target ({}) names a column more than once",
                on_conflict.conflict_columns.join(", ")
            )));
        }
        let constraint = constraints
            .iter()
            .find(|constraint| {
                constraint.columns.len() == target.len()
                    && constraint
                        .columns
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        == target
            })
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "ON CONFLICT target ({}) does not match a PRIMARY KEY or UNIQUE constraint",
                    on_conflict.conflict_columns.join(", ")
                ))
            })?;
        let Some(conflict_values) = key_constraint_values(constraint, document) else {
            return Ok(None);
        };
        return engine.find_conflict(table, &constraint.columns, &conflict_values);
    }

    for constraint in &constraints {
        let Some(values) = key_constraint_values(constraint, document) else {
            continue;
        };
        if let Some(doc_id) = engine.find_conflict(table, &constraint.columns, &values)? {
            return Ok(Some(doc_id));
        }
    }
    Ok(None)
}

enum InsertConflictResolution {
    Insert,
    Skip,
    Updated { doc_id: DocId, document: Document },
}

fn resolve_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<InsertConflictResolution, SQLError> {
    let Some(existing_id) = find_insert_conflict(engine, table, on_conflict, document)? else {
        return Ok(InsertConflictResolution::Insert);
    };
    match &on_conflict.action {
        ConflictActionPlan::Nothing => Ok(InsertConflictResolution::Skip),
        ConflictActionPlan::Update {
            assignments,
            predicate,
        } => {
            let existing_doc = engine
                .get_document(table, existing_id)?
                .ok_or_else(|| missing_document_error("INSERT ON CONFLICT", table, existing_id))?;
            let mut conflict_ctx_doc = existing_doc.clone();
            for (column, value) in &existing_doc {
                conflict_ctx_doc.insert(format!("{table}.{column}"), value.clone());
            }
            for (column, value) in document {
                conflict_ctx_doc.insert(format!("excluded.{column}"), value.clone());
            }
            if let Some(predicate) = predicate {
                let keep =
                    eval_mutation_expr(engine, scope, predicate, Some(&conflict_ctx_doc), params)?;
                if !uqa_sql::expr::truthy(&keep) {
                    return Ok(InsertConflictResolution::Skip);
                }
            }
            let mut updated_doc = existing_doc.clone();
            for assignment in assignments {
                let value = coerce_to_column_type(
                    engine,
                    table,
                    &assignment.column,
                    eval_mutation_expr(
                        engine,
                        scope,
                        &assignment.value,
                        Some(&conflict_ctx_doc),
                        params,
                    )?,
                )?;
                updated_doc.insert(assignment.column.clone(), value);
            }
            let rewritten_doc_id = rewrite_document_with_referential_actions(
                engine,
                table,
                existing_id,
                &existing_doc,
                updated_doc.clone(),
                params,
            )?;
            Ok(InsertConflictResolution::Updated {
                doc_id: rewritten_doc_id,
                document: updated_doc,
            })
        }
    }
}

fn build_returning_row(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    document: &Document,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    let mut row_doc = document.clone();
    row_doc.insert(DOC_ID_COLUMN.into(), doc_id_value(doc_id)?);
    build_projection_row_with_ctes(engine, &row_doc, returning, params, ctes).map_err(|err| {
        SQLError::Internal(format!(
            "RETURNING projection failed for table `{table}` doc {doc_id}: {err}"
        ))
    })
}

fn dml_returning_result(
    engine: &Engine,
    table: &str,
    returning: &[ProjectionPlan],
    rows: Vec<ResultRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    Ok(SQLResult {
        columns: expand_star_columns(
            projection_columns(returning),
            returning,
            engine,
            Some(table),
        )?,
        rows,
        affected_rows,
    })
}

fn document_supplied_id(
    document: &Document,
    id_column: &str,
    auto_increment: bool,
) -> Result<Option<DocId>, SQLError> {
    match document.get(id_column) {
        Some(Value::Int(value)) if *value >= 0 => Ok(Some(*value as DocId)),
        Some(Value::Null) | None => Ok(None),
        Some(other) if auto_increment => Err(SQLError::TypeMismatch(format!(
            "auto-increment id must be an integer, got {other:?}"
        ))),
        Some(_) => Ok(None),
    }
}

// -------------------------------------------------------------------------

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

pub(super) fn run_insert(
    engine: &Engine,
    stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_insert_inner(engine, &stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut scope = CteScope::new();
    super::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut scope)?;
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    // Resolve the table's primary-key column name. Auto-increment
    // (SERIAL / BIGSERIAL) wins; otherwise the scalar PRIMARY KEY
    // column wins; otherwise use the conventional legacy `id` slot.
    // Both VALUES and SELECT sources must derive the internal doc id
    // from this same column or later primary-key rewrites can address a
    // different row than the one that was inserted.
    let (auto_id_col, id_column) = insert_identity_columns(engine, &stmt.table, "INSERT")?;
    if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        validate_mutation_columns(
            engine,
            &stmt.table,
            assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            "INSERT ON CONFLICT DO UPDATE",
        )?;
    }

    // INSERT ... SELECT: materialise the inner SELECT first, then
    // route each row through the standard add_document path under
    // the named columns.
    if let Some(source) = stmt.source.as_deref() {
        let result =
            super::select::execute_query_plan_with_ctes(engine, source, params, &mut scope)?;
        let columns: Vec<String> = if stmt.columns.is_empty() {
            let target_columns = engine
                .try_table_columns(&stmt.table)
                .map_err(|err| dml_storage_error("INSERT SELECT", err))?;
            if target_columns.is_empty() {
                result.columns.clone()
            } else {
                target_columns
            }
        } else {
            stmt.columns.clone()
        };
        validate_mutation_columns(
            engine,
            &stmt.table,
            columns.iter().map(String::as_str),
            "INSERT SELECT",
        )?;
        if result.columns.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "INSERT SELECT width {} != column count {}",
                result.columns.len(),
                columns.len()
            )));
        }
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = engine.cancellation_token();
        for source_row in result.rows {
            cancel.check()?;
            let mut document = Document::new();
            for (idx, col) in columns.iter().enumerate() {
                let source_col = &result.columns[idx];
                if let Some(v) = source_row.get(source_col) {
                    document.insert(
                        col.clone(),
                        coerce_to_column_type(engine, &stmt.table, col, v.clone())?,
                    );
                }
            }

            // INSERT ... SELECT must follow the same constraint and
            // conflict path as INSERT ... VALUES.  In particular,
            // defaults participate in conflict-key inference, while
            // non-key constraints are checked before a conflicting row
            // can be rewritten or skipped.
            apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
            validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
            if let Some(on_conflict) = stmt.on_conflict.as_ref() {
                match resolve_insert_conflict(
                    engine,
                    &stmt.table,
                    on_conflict,
                    &document,
                    params,
                    &scope,
                )? {
                    InsertConflictResolution::Insert => {}
                    InsertConflictResolution::Skip => continue,
                    InsertConflictResolution::Updated { doc_id, document } => {
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_returning_row(
                                engine,
                                &stmt.table,
                                doc_id,
                                &document,
                                &stmt.returning,
                                params,
                                &scope,
                            )?);
                        }
                        affected += 1;
                        continue;
                    }
                }
            }

            let supplied_id = document_supplied_id(
                &document,
                &id_column,
                auto_id_col.as_deref() == Some(id_column.as_str()),
            )?;
            let doc_id = match supplied_id {
                Some(doc_id) => doc_id,
                None => engine.allocate_next_id(&stmt.table)?,
            };
            if auto_id_col.as_deref() == Some(id_column.as_str()) {
                document.insert(id_column.clone(), doc_id_value(doc_id)?);
            }
            engine
                .advance_next_id(&stmt.table, doc_id)
                .map_err(|err| dml_storage_error("INSERT SELECT", err))?;
            let document = insert_document_with_constraints(
                engine,
                &stmt.table,
                doc_id,
                document,
                params,
                false,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &stmt.table,
                    doc_id,
                    &document,
                    &stmt.returning,
                    params,
                    &scope,
                )?);
            }
            affected += 1;
        }
        if !stmt.returning.is_empty() {
            return dml_returning_result(
                engine,
                &stmt.table,
                &stmt.returning,
                returning_rows,
                affected,
            );
        }
        return Ok(SQLResult::from_affected(affected));
    }

    // Whether a user-supplied id value is covered by the strict UNIQUE
    // pre-check below. The legacy bare-`id` fallback maps a plain
    // column onto the doc id without any uniqueness guarantee, so
    // writes through it may replace an existing document.
    let id_column_is_unique_key = engine
        .try_unique_columns(&stmt.table)
        .map_err(|err| dml_storage_error("INSERT", err))?
        .contains(&id_column);

    let columns: Vec<String> = if stmt.columns.is_empty() {
        // INSERT without explicit column list: project the table schema.
        let cols = engine
            .try_table_columns(&stmt.table)
            .map_err(|err| dml_storage_error("INSERT", err))?;
        if cols.is_empty() {
            return Err(SQLError::Unsupported(
                "INSERT without column list against a table with no schema".into(),
            ));
        }
        cols
    } else {
        stmt.columns.clone()
    };
    validate_mutation_columns(
        engine,
        &stmt.table,
        columns.iter().map(String::as_str),
        "INSERT",
    )?;

    // No explicit id and no auto-increment column: allocate a synthetic
    // u64 doc_id at insert time. Mirrors the canonical UQA behavior, which
    // treats every table as having an implicit doc_id even when the
    // schema declares no primary key.

    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    for row in &stmt.rows {
        cancel.check()?;
        if row.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "row width {} != column count {}",
                row.len(),
                columns.len()
            )));
        }
        let mut document = Document::new();
        for (i, col) in columns.iter().enumerate() {
            let mut v = eval_mutation_expr(engine, &scope, &row[i], None, params)?;
            v = coerce_to_column_type(engine, &stmt.table, col, v)?;
            document.insert(col.clone(), v);
        }

        // Defaults and all non-key constraints are shared with
        // INSERT ... SELECT. Resolve the internal id only afterwards so
        // an integer primary-key DEFAULT cannot diverge from the row's
        // physical document id.
        apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
        validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
        let doc_id = document_supplied_id(
            &document,
            &id_column,
            auto_id_col.as_deref() == Some(id_column.as_str()),
        )?;

        // UNIQUE constraint validation -- before any conflict
        // resolution, every UNIQUE / PRIMARY KEY column whose value
        // is non-null must not already exist in another row. The
        // ON CONFLICT branch below intentionally skips this check
        // because that path explicitly chooses a merge action.
        if stmt.on_conflict.is_none() {
            validate_key_constraints(engine, &stmt.table, &document, None)?;
        }

        // ON CONFLICT lookup -- check whether a row with matching
        // conflict-target columns already exists. The conflict
        // columns may include the primary key, so we collect their
        // current values from the row being inserted.
        if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            match resolve_insert_conflict(
                engine,
                &stmt.table,
                on_conflict,
                &document,
                params,
                &scope,
            )? {
                InsertConflictResolution::Insert => {}
                InsertConflictResolution::Skip => continue,
                InsertConflictResolution::Updated { doc_id, document } => {
                    if !stmt.returning.is_empty() {
                        returning_rows.push(build_returning_row(
                            engine,
                            &stmt.table,
                            doc_id,
                            &document,
                            &stmt.returning,
                            params,
                            &scope,
                        )?);
                    }
                    affected += 1;
                    continue;
                }
            }
        }

        let supplied_id = doc_id.is_some();
        let doc_id = if let Some(id) = doc_id {
            id
        } else {
            let id = engine.allocate_next_id(&stmt.table)?;
            // Only stamp the allocated id back onto the document when
            // the primary-key column is auto-increment. For non-auto
            // primary keys (TEXT, UUID, ...) the user-supplied value
            // already lives in `document[id_column]` and must be
            // preserved -- the synthetic u64 stays internal.
            if auto_id_col.as_deref() == Some(id_column.as_str()) {
                document.insert(id_column.clone(), doc_id_value(id)?);
            }
            id
        };
        engine
            .advance_next_id(&stmt.table, doc_id)
            .map_err(|err| dml_storage_error("INSERT", err))?;
        // A document is known new when nothing can have claimed its doc
        // id: an allocator-issued id is fresh by construction, and a
        // user-supplied id is proven absent by the strict UNIQUE
        // pre-check above - which only covers the id column when it is
        // an actual key (not the legacy bare-`id` fallback). ON
        // CONFLICT skips the pre-check, and its target can miss the
        // primary key, so that path keeps the replacement-aware write.
        let known_new = stmt.on_conflict.is_none() && (!supplied_id || id_column_is_unique_key);
        let document = insert_document_with_constraints(
            engine,
            &stmt.table,
            doc_id,
            document,
            params,
            known_new,
        )?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                doc_id,
                &document,
                &stmt.returning,
                params,
                &scope,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

/// `known_new` asserts the caller proved `doc_id` absent (fresh
/// allocator id, or a VALUES insert whose strict uniqueness pre-check
/// ran). Paths that can legitimately overwrite an existing document -
/// MERGE inserts and INSERT ... SELECT with caller-supplied ids - must
/// pass `false` so value-index maintenance unindexes the old values.
fn insert_document_with_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    mut document: Document,
    params: &[SQLParam],
    known_new: bool,
) -> Result<Document, SQLError> {
    apply_missing_column_defaults(engine, table, &mut document, params)?;
    validate_document_constraints(engine, table, &document, params, None)?;
    if known_new {
        engine.add_document_with_vector_values_known_new(
            table,
            doc_id,
            document.clone(),
            document_vectors(engine, table, &document)?,
        )?;
    } else {
        engine.add_document_with_vector_values(
            table,
            doc_id,
            document.clone(),
            document_vectors(engine, table, &document)?,
        )?;
    }
    Ok(document)
}

fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for col in engine
        .try_table_columns(table)
        .map_err(|err| dml_storage_error("INSERT defaults", err))?
    {
        if document.contains_key(&col) {
            continue;
        }
        if let Some(default_expr) = engine
            .try_column_default_expr(table, &col)
            .map_err(|err| dml_storage_error("INSERT defaults", err))?
        {
            let value = coerce_to_column_type(
                engine,
                table,
                &col,
                eval_lowered_expression(engine, &default_expr, None, params)?,
            )?;
            document.insert(col, value);
        }
    }
    Ok(())
}

fn document_vectors(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    let mut vectors = BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = engine
            .column_type(table, field)
            .map_err(|err| dml_storage_error("vector extraction", err))?
        else {
            continue;
        };
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(field.clone(), index_vectors_for_type(value, &ty)?);
        }
    }
    Ok(vectors)
}

pub(super) fn index_vectors_for_type(
    value: &Value,
    ty: &ColumnType,
) -> Result<Vec<Vec<f32>>, SQLError> {
    // SQL VECTOR/TENSOR columns are nullable unless their declaration says
    // otherwise. A NULL value therefore means that the row has no vectors to
    // index; it is not a malformed vector. Returning an empty replacement set
    // also clears any vectors left by an UPDATE ... SET field = NULL while
    // retaining strict validation for every non-NULL value.
    if matches!(value, Value::Null) {
        return Ok(Vec::new());
    }
    match ty {
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(vec![vector])
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(tensor)
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "{} is not vector-indexable",
            column_type_name(ty)
        ))),
    }
}
