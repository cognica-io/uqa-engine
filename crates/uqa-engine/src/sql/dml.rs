//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{
    build_join_rows_with_ctes, build_projection_row_with_ctes, coerce_to_column_type,
    column_type_name, expand_star_columns, prefix_row, projection_columns,
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
    let source_rows = build_join_rows_with_ctes(engine, &stmt.source, params, &mut ctes)?;
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();

    struct Pairing {
        doc_id: Option<uqa_core::DocId>,
        target_row: ResultRow,
        source_row: Option<ResultRow>,
    }
    let mut pairings: Vec<Pairing> = Vec::new();
    let mut matched_source: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    for doc_id in &engine.table_doc_ids(&target_table) {
        let Some(doc) = engine.get_document(&target_table, *doc_id) else {
            continue;
        };
        let target_row = prefix_row(&target_qual, &doc);
        let mut paired_idx: Option<usize> = None;
        for (idx, src) in source_rows.iter().enumerate() {
            if matched_source.contains(&idx) {
                continue;
            }
            let mut joined = ResultRow::new();
            for (k, v) in &target_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in src {
                joined.insert(k.clone(), v.clone());
            }
            if truthy(&eval_mutation_expr(
                engine,
                &ctes,
                &stmt.join_condition,
                Some(&joined),
                params,
            )?) {
                paired_idx = Some(idx);
                matched_source.insert(idx);
                break;
            }
        }
        // Skip target rows that don't pair with any source row --
        // MERGE only emits an action when the join condition holds.
        if let Some(idx) = paired_idx {
            pairings.push(Pairing {
                doc_id: Some(*doc_id),
                target_row,
                source_row: Some(source_rows[idx].clone()),
            });
        }
    }
    for (idx, src) in source_rows.iter().enumerate() {
        if matched_source.contains(&idx) {
            continue;
        }
        pairings.push(Pairing {
            doc_id: None,
            target_row: ResultRow::new(),
            source_row: Some(src.clone()),
        });
    }

    for pair in pairings {
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
                        let Some(mut doc) = engine.get_document(&target_table, doc_id) else {
                            break;
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
                        rewrite_document_with_referential_actions(
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
                                    doc_id,
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
                            engine.get_document(&target_table, doc_id)
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
                    let id_col = engine.auto_increment_column(&target_table);
                    let doc_id = match id_col.as_deref().and_then(|c| document.get(c)) {
                        Some(Value::Int(n)) if *n >= 0 => *n as u64,
                        _ => engine.allocate_next_id(&target_table)?,
                    };
                    if let Some(c) = id_col.as_deref() {
                        document.insert(c.to_string(), Value::Int(doc_id as i64));
                    }
                    engine.advance_next_id(&target_table, doc_id);
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
        return Ok(dml_returning_result(
            engine,
            &target_table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
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
    row_doc.insert(DOC_ID_COLUMN.into(), Value::Int(input.doc_id as i64));
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
        let filter = stmt.predicate.as_ref().expect("preselected requires WHERE");
        super::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
    } else {
        engine.table_doc_ids(&stmt.table)
    };
    for doc_id in doc_ids {
        cancel.check()?;
        let Some(mut doc) = engine.get_document(&stmt.table, doc_id) else {
            continue;
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
        rewrite_document_with_referential_actions(
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
                doc_id,
                &doc,
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
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
    if !can_patch_update_without_full_row(engine, &stmt.table, &updates) {
        return Ok(None);
    }
    if matches!(lookup_value, Value::Null) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    if !point_lookup_field_is_unique(engine, &stmt.table, &lookup_field) {
        return Ok(None);
    }
    let Some(doc_id) = engine.find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)
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
        if let Some(ty) = engine.column_type(&stmt.table, &assignment.column) {
            if let Ok(values) = index_vectors_for_type(&value, &ty) {
                vectors.insert(assignment.column.clone(), values);
            }
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
) -> bool {
    if !engine.check_constraints(table).is_empty() {
        return false;
    }
    let update_keys: BTreeSet<&str> = updates.keys().map(String::as_str).collect();
    if engine
        .describe_table(table)
        .unwrap_or_default()
        .iter()
        .any(|col| {
            col.not_null
                && !col.auto_increment
                && matches!(updates.get(&col.name), Some(Value::Null))
        })
    {
        return false;
    }
    if engine
        .unique_columns(table)
        .iter()
        .any(|column| update_keys.contains(column.as_str()))
    {
        return false;
    }
    if engine.foreign_keys(table).iter().any(|fk| {
        fk.local_columns
            .iter()
            .any(|column| update_keys.contains(column.as_str()))
    }) {
        return false;
    }
    if referrers_to_for_actions(engine, table)
        .iter()
        .any(|(_, fk)| {
            fk.ref_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return false;
    }
    true
}

fn point_lookup_field_is_unique(engine: &Engine, table: &str, lookup_field: &str) -> bool {
    engine
        .describe_table(table)
        .unwrap_or_default()
        .iter()
        .any(|column| column.name == lookup_field && (column.primary_key || column.unique))
}

fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for col_def in engine.describe_table(table).unwrap_or_default() {
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

    for (cname, expr) in engine.check_constraints(table) {
        let result = eval_lowered_expression(engine, &expr, Some(document), params)?;
        if !uqa_sql::expr::truthy(&result) {
            let label = cname.unwrap_or_else(|| "<unnamed>".into());
            return Err(SQLError::TypeMismatch(format!(
                "CHECK constraint `{label}` violated in table `{table}`"
            )));
        }
    }

    for fk in engine.foreign_keys(table) {
        let Some(local_values) = foreign_key_lookup_values(&fk, document)? else {
            continue;
        };
        if engine
            .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)
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

    for col in engine.unique_columns(table) {
        let Some(value) = document.get(&col).cloned() else {
            continue;
        };
        if matches!(value, Value::Null) {
            continue;
        }
        if let Some(conflict_id) = engine.find_conflict(
            table,
            std::slice::from_ref(&col),
            std::slice::from_ref(&value),
        ) {
            if conflict_id != doc_id {
                return Err(SQLError::TypeMismatch(format!(
                    "UNIQUE constraint violated: duplicate value for column `{col}` in table `{table}`"
                )));
            }
        }
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
) -> Result<(), SQLError> {
    validate_document_constraints(engine, table, doc_id, &new_doc, params)?;
    match integer_primary_key_doc_id(engine, table, &new_doc) {
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
                document_vectors(engine, table, &new_doc),
            )?;
            engine.advance_next_id(table, new_id);
        }
        _ => engine.rewrite_document(table, doc_id, new_doc.clone())?,
    }
    apply_referenced_key_update_actions(engine, table, old_doc, &new_doc, params)
}

fn integer_primary_key_doc_id(engine: &Engine, table: &str, doc: &Document) -> Option<DocId> {
    let cols = engine.describe_table(table)?;
    let pk = cols
        .iter()
        .find(|c| c.primary_key && matches!(c.ty, uqa_sql::ast::ColumnType::Integer))?;
    match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    }
}

fn apply_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(engine, table) {
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
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &old_values);
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

fn referrers_to_for_actions(engine: &Engine, table: &str) -> Vec<(String, ForeignKey)> {
    let mut out = Vec::new();
    for other in engine.table_names() {
        for fk in engine.foreign_keys(&other) {
            if fk.ref_table == table {
                out.push((other.clone(), fk));
            }
        }
    }
    out
}

fn referencing_rows(
    engine: &Engine,
    table: &str,
    local_columns: &[String],
    key_values: &[Value],
) -> Vec<(DocId, Document)> {
    if local_columns.is_empty() || local_columns.len() != key_values.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        let matches = local_columns
            .iter()
            .zip(key_values.iter())
            .all(|(col, want)| doc.get(col).cloned().unwrap_or(Value::Null) == *want);
        if matches {
            out.push((doc_id, doc));
        }
    }
    out
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
                if let Some(expr) = engine.column_default_expr(table, column) {
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
    let from_rows = build_join_rows_with_ctes(engine, from_clause, params, ctes)?;
    let cancel = engine.cancellation_token();
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let target = stmt.table.clone();
    let target_doc_ids = engine.table_doc_ids(&target);
    for doc_id in target_doc_ids {
        cancel.check()?;
        let Some(mut doc) = engine.get_document(&target, doc_id) else {
            continue;
        };
        let original_doc = doc.clone();
        let mut applied = false;
        for from_row in &from_rows {
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
            for (k, v) in from_row {
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
            rewrite_document_with_referential_actions(
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
                    doc_id,
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
        return Ok(dml_returning_result(
            engine,
            &target,
            &stmt.returning,
            returning_rows,
            affected,
        ));
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
    let using_rows: Option<Vec<ResultRow>> = match stmt.source.as_deref() {
        Some(source) => Some(build_join_rows_with_ctes(
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
        let filter = stmt.predicate.as_ref().expect("preselected requires WHERE");
        super::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
    } else {
        engine.table_doc_ids(&stmt.table)
    };
    for doc_id in doc_ids {
        cancel.check()?;
        if preselected && stmt.returning.is_empty() {
            // No RETURNING and the filter already matched: the
            // document body is not needed at all.
            to_delete.push(doc_id);
            continue;
        }
        let Some(doc) = engine.get_document(&stmt.table, doc_id) else {
            continue;
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
                for using_row in rows {
                    let mut joined = ResultRow::new();
                    for (k, v) in &doc {
                        joined.insert(k.clone(), v.clone());
                        joined.insert(format!("{}.{k}", stmt.table), v.clone());
                    }
                    for (k, v) in using_row {
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
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
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
    let Some(target) = engine.get_document(table, doc_id) else {
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
    for (ref_table, fk) in referrers_to_for_actions(engine, table) {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| target.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &key_values);
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
) -> Option<DocId> {
    if !on_conflict.conflict_columns.is_empty() {
        let conflict_values: Vec<Value> = on_conflict
            .conflict_columns
            .iter()
            .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        return engine.find_conflict(table, &on_conflict.conflict_columns, &conflict_values);
    }

    for col in engine.unique_columns(table) {
        let value = document.get(&col).cloned().unwrap_or(Value::Null);
        if matches!(value, Value::Null) {
            continue;
        }
        if let Some(doc_id) = engine.find_conflict(
            table,
            std::slice::from_ref(&col),
            std::slice::from_ref(&value),
        ) {
            return Some(doc_id);
        }
    }
    None
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
    row_doc.insert(DOC_ID_COLUMN.into(), Value::Int(doc_id as i64));
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
) -> SQLResult {
    SQLResult {
        columns: expand_star_columns(
            projection_columns(returning),
            returning,
            engine,
            Some(table),
        ),
        rows,
        affected_rows,
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
    // INSERT ... SELECT: materialise the inner SELECT first, then
    // route each row through the standard add_document path under
    // the named columns.
    if let Some(source) = stmt.source.as_deref() {
        let result =
            super::select::execute_query_plan_with_ctes(engine, source, params, &mut scope)?;
        let columns: Vec<String> = if stmt.columns.is_empty() {
            result.columns.clone()
        } else {
            stmt.columns.clone()
        };
        if result.columns.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "INSERT SELECT width {} != column count {}",
                result.columns.len(),
                columns.len()
            )));
        }
        let auto_id_col = engine.auto_increment_column(&stmt.table);
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = engine.cancellation_token();
        for source_row in result.rows {
            cancel.check()?;
            let mut document = Document::new();
            for (idx, col) in columns.iter().enumerate() {
                let source_col = if stmt.columns.is_empty() {
                    col
                } else {
                    &result.columns[idx]
                };
                if let Some(v) = source_row.get(source_col) {
                    document.insert(
                        col.clone(),
                        coerce_to_column_type(engine, &stmt.table, col, v.clone())?,
                    );
                }
            }
            let doc_id = match auto_id_col.as_deref().and_then(|c| document.get(c)) {
                Some(Value::Int(n)) if *n >= 0 => *n as u64,
                _ => engine.allocate_next_id(&stmt.table)?,
            };
            if let Some(c) = auto_id_col.as_deref() {
                document.insert(c.to_string(), Value::Int(doc_id as i64));
            }
            engine.advance_next_id(&stmt.table, doc_id);
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
            return Ok(dml_returning_result(
                engine,
                &stmt.table,
                &stmt.returning,
                returning_rows,
                affected,
            ));
        }
        return Ok(SQLResult::from_affected(affected));
    }

    let auto_id_col = engine.auto_increment_column(&stmt.table);
    // Resolve the table's primary-key column name. Auto-increment
    // (SERIAL / BIGSERIAL) wins; otherwise the first PRIMARY KEY
    // column wins; otherwise we fall back to the conventional "id"
    // slot so legacy tests keep passing.
    let id_column = auto_id_col.clone().or_else(|| {
        engine
            .describe_table(&stmt.table)
            .and_then(|cols| cols.into_iter().find(|c| c.primary_key))
            .map(|c| c.name)
    });
    let id_column = id_column.unwrap_or_else(|| "id".into());
    // Whether a user-supplied id value is covered by the strict UNIQUE
    // pre-check below. The legacy bare-`id` fallback maps a plain
    // column onto the doc id without any uniqueness guarantee, so
    // writes through it may replace an existing document.
    let id_column_is_unique_key = engine.unique_columns(&stmt.table).contains(&id_column);

    let columns: Vec<String> = if stmt.columns.is_empty() {
        // INSERT without explicit column list: project the table schema.
        let cols = engine.table_columns(&stmt.table);
        if cols.is_empty() {
            return Err(SQLError::Unsupported(
                "INSERT without column list against a table with no schema".into(),
            ));
        }
        cols
    } else {
        stmt.columns.clone()
    };

    let id_index = columns.iter().position(|c| c == &id_column);
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
        let mut doc_id: Option<u64> = None;
        for (i, col) in columns.iter().enumerate() {
            let mut v = eval_mutation_expr(engine, &scope, &row[i], None, params)?;
            v = coerce_to_column_type(engine, &stmt.table, col, v)?;
            if Some(i) == id_index {
                // Auto-increment primary keys must be integers. A
                // non-auto-increment primary key (TEXT, UUID, ...) keeps
                // the user value in the document and the engine still
                // allocates a synthetic u64 doc_id for posting-list
                // bookkeeping. UNIQUE / PRIMARY KEY enforcement runs
                // through `engine.unique_columns` regardless.
                let is_auto = auto_id_col.as_deref() == Some(id_column.as_str());
                doc_id = match &v {
                    Value::Int(n) if *n >= 0 => Some(*n as u64),
                    Value::Null => None,
                    other if is_auto => {
                        return Err(SQLError::TypeMismatch(format!(
                            "auto-increment id must be an integer, got {other:?}"
                        )));
                    }
                    _ => None,
                };
            }
            document.insert(col.clone(), v);
        }

        // DEFAULT expression -- evaluate when the column was absent
        // from the INSERT column list. Mirrors the canonical UQA behavior's
        // _evaluate_default. The engine hook is in scope so DEFAULT
        // nextval('seq') resolves through the sequence store.
        for col in engine.table_columns(&stmt.table) {
            if document.contains_key(&col) {
                continue;
            }
            if let Some(default_expr) = engine.column_default_expr(&stmt.table, &col) {
                let v = coerce_to_column_type(
                    engine,
                    &stmt.table,
                    &col,
                    eval_lowered_expression(engine, &default_expr, None, params)?,
                )?;
                document.insert(col.clone(), v);
            }
        }

        // NOT NULL validation -- after defaults are applied, every
        // declared NOT NULL column must have a non-null value.
        // Auto-increment columns are exempt because the engine fills
        // them in below.
        for col_def in engine.describe_table(&stmt.table).unwrap_or_default() {
            if !col_def.not_null || col_def.auto_increment {
                continue;
            }
            match document.get(&col_def.name) {
                Some(Value::Null) | None => {
                    return Err(SQLError::TypeMismatch(format!(
                        "NOT NULL constraint violated: column `{}` in table `{}`",
                        col_def.name, stmt.table
                    )));
                }
                _ => {}
            }
        }

        // CHECK constraints -- evaluate every column-level + table-
        // level CHECK against the row and reject when any returns a
        // non-truthy value.
        for (cname, expr) in engine.check_constraints(&stmt.table) {
            let result = eval_lowered_expression(engine, &expr, Some(&document), params)?;
            if !uqa_sql::expr::truthy(&result) {
                let label = cname.unwrap_or_else(|| "<unnamed>".into());
                return Err(SQLError::TypeMismatch(format!(
                    "CHECK constraint `{label}` violated in table `{}`",
                    stmt.table
                )));
            }
        }

        // FOREIGN KEY constraints -- MATCH SIMPLE skips any tuple
        // containing NULL, while MATCH FULL requires either every
        // local key column to be NULL or none of them to be NULL.
        for fk in engine.foreign_keys(&stmt.table) {
            let Some(local_values) = foreign_key_lookup_values(&fk, &document)? else {
                continue;
            };
            if engine
                .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)
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

        // UNIQUE constraint validation -- before any conflict
        // resolution, every UNIQUE / PRIMARY KEY column whose value
        // is non-null must not already exist in another row. The
        // ON CONFLICT branch below intentionally skips this check
        // because that path explicitly chooses a merge action.
        if stmt.on_conflict.is_none() {
            for col in engine.unique_columns(&stmt.table) {
                let Some(value) = document.get(&col).cloned() else {
                    continue;
                };
                if matches!(value, Value::Null) {
                    continue;
                }
                if engine
                    .find_conflict(
                        &stmt.table,
                        std::slice::from_ref(&col),
                        std::slice::from_ref(&value),
                    )
                    .is_some()
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "UNIQUE constraint violated: duplicate value for column `{col}` in table `{}`",
                        stmt.table
                    )));
                }
            }
        }

        // ON CONFLICT lookup -- check whether a row with matching
        // conflict-target columns already exists. The conflict
        // columns may include the primary key, so we collect their
        // current values from the row being inserted.
        if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            if let Some(existing_id) =
                find_insert_conflict(engine, &stmt.table, on_conflict, &document)
            {
                match &on_conflict.action {
                    ConflictActionPlan::Nothing => {
                        continue;
                    }
                    ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } => {
                        let existing_doc = engine
                            .get_document(&stmt.table, existing_id)
                            .unwrap_or_default();
                        let mut conflict_ctx_doc = existing_doc.clone();
                        for (col, value) in &existing_doc {
                            conflict_ctx_doc.insert(format!("{}.{col}", stmt.table), value.clone());
                        }
                        for (col, value) in &document {
                            conflict_ctx_doc.insert(format!("excluded.{col}"), value.clone());
                        }
                        if let Some(pred) = predicate {
                            let keep = eval_mutation_expr(
                                engine,
                                &scope,
                                pred,
                                Some(&conflict_ctx_doc),
                                params,
                            )?;
                            if !uqa_sql::expr::truthy(&keep) {
                                continue;
                            }
                        }
                        let mut updated_doc = existing_doc.clone();
                        for assignment in assignments {
                            let v = coerce_to_column_type(
                                engine,
                                &stmt.table,
                                &assignment.column,
                                eval_mutation_expr(
                                    engine,
                                    &scope,
                                    &assignment.value,
                                    Some(&conflict_ctx_doc),
                                    params,
                                )?,
                            )?;
                            updated_doc.insert(assignment.column.clone(), v.clone());
                        }
                        rewrite_document_with_referential_actions(
                            engine,
                            &stmt.table,
                            existing_id,
                            &existing_doc,
                            updated_doc.clone(),
                            params,
                        )?;
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_returning_row(
                                engine,
                                &stmt.table,
                                existing_id,
                                &updated_doc,
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
                document.insert(id_column.clone(), Value::Int(id as i64));
            }
            id
        };
        engine.advance_next_id(&stmt.table, doc_id);
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
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
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
    validate_document_constraints(engine, table, doc_id, &document, params)?;
    if known_new {
        engine.add_document_with_vector_values_known_new(
            table,
            doc_id,
            document.clone(),
            document_vectors(engine, table, &document),
        )?;
    } else {
        engine.add_document_with_vector_values(
            table,
            doc_id,
            document.clone(),
            document_vectors(engine, table, &document),
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
    for col in engine.table_columns(table) {
        if document.contains_key(&col) {
            continue;
        }
        if let Some(default_expr) = engine.column_default_expr(table, &col) {
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
) -> BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>> {
    let mut vectors = BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = engine.column_type(table, field) else {
            continue;
        };
        if let Ok(values) = index_vectors_for_type(value, &ty) {
            vectors.insert(field.clone(), values);
        }
    }
    vectors
}

pub(super) fn index_vectors_for_type(
    value: &Value,
    ty: &ColumnType,
) -> Result<Vec<Vec<f32>>, SQLError> {
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
