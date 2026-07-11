//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{
    build_join_rows_with_ctes, build_projection_row, coerce_to_column_type, column_type_name, eval,
    execute_select, expand_star_columns, materialize_cte_list, prefix_row, projection_columns,
    validate_vector_dimensions, value_to_tensor, value_to_vector, BTreeMap, BTreeSet, BinaryOp,
    ColumnType, CteScope, DeleteStmt, DocId, Document, Engine, EvalContext, Expr, ForeignKey,
    ForeignKeyAction, ForeignKeyMatch, InsertStmt, Projection, ResultRow,
    RowIndependentUpdateValues, SQLError, SQLParam, SQLResult, ScopedEngineHook, UpdateStmt, Value,
    DOC_ID_COLUMN, MERGE_ACTION_COLUMN,
};

#[allow(clippy::too_many_lines)]
pub(super) fn run_merge(
    engine: &Engine,
    stmt: uqa_sql::ast::MergeStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_merge_inner(engine, stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_merge_inner(
    engine: &Engine,
    stmt: uqa_sql::ast::MergeStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::ast::MergeWhen;
    use uqa_sql::expr::{eval, truthy, EvalContext};
    let target_table = stmt.target.clone();
    let target_qual = stmt
        .target_alias
        .clone()
        .unwrap_or_else(|| target_table.clone());
    let mut ctes = CteScope::new();
    let source_rows = build_join_rows_with_ctes(engine, &stmt.source, params, &mut ctes)?;
    let eval_hook: &dyn uqa_sql::expr::EngineHook = engine;
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
            let ctx = EvalContext::new(Some(&joined), params).with_engine(eval_hook);
            if truthy(&eval(&stmt.join_condition, &ctx)?) {
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
                MergeWhen::UpdateMatched { condition, .. } if matched => {
                    (condition.as_ref(), 0_u8, true)
                }
                MergeWhen::DeleteMatched { condition } if matched => (condition.as_ref(), 1, true),
                MergeWhen::NothingMatched { condition } if matched => (condition.as_ref(), 2, true),
                MergeWhen::InsertNotMatched { condition, .. } if !matched => {
                    (condition.as_ref(), 3, true)
                }
                MergeWhen::NothingNotMatched { condition } if !matched => {
                    (condition.as_ref(), 2, true)
                }
                _ => (None, 0, false),
            };
            if !applies {
                continue;
            }
            if let Some(c) = condition {
                let ctx = EvalContext::new(Some(&joined), params).with_engine(eval_hook);
                if !truthy(&eval(c, &ctx)?) {
                    continue;
                }
            }
            match (action_idx, clause) {
                (0, MergeWhen::UpdateMatched { assignments, .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        let Some(mut doc) = engine.get_document(&target_table, doc_id) else {
                            break;
                        };
                        let original_doc = doc.clone();
                        let ctx = EvalContext::new(Some(&joined), params).with_engine(eval_hook);
                        for (col, expr) in assignments {
                            let value = coerce_to_column_type(
                                engine,
                                &target_table,
                                col,
                                eval(expr, &ctx)?,
                            )?;
                            doc.insert(col.clone(), value);
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
                            )?);
                        }
                    }
                }
                (1, MergeWhen::DeleteMatched { .. }) => {
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
                            )?);
                        }
                    }
                }
                (
                    3,
                    MergeWhen::InsertNotMatched {
                        columns, values, ..
                    },
                ) => {
                    let ctx = EvalContext::new(Some(&joined), params).with_engine(eval_hook);
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
                            eval(&values[i], &ctx)?,
                        )?;
                        document.insert(col.clone(), v);
                    }
                    let id_col = engine.auto_increment_column(&target_table);
                    let doc_id = match id_col.as_deref().and_then(|c| document.get(c)) {
                        Some(Value::Int(n)) if *n >= 0 => *n as u64,
                        _ => engine.allocate_next_id(&target_table)?,
                    };
                    if let Some(c) = id_col.as_deref() {
                        document.insert(c.into(), Value::Int(doc_id as i64));
                    }
                    engine.advance_next_id(&target_table, doc_id);
                    let inserted = insert_document_with_constraints(
                        engine,
                        &target_table,
                        doc_id,
                        document,
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
                                document: &inserted,
                                source_row: pair.source_row.as_ref(),
                                action: "INSERT",
                            },
                            &stmt.returning,
                            params,
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
    returning: &[Projection],
    params: &[SQLParam],
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
    build_projection_row(Some(engine), &row_doc, returning, params).map_err(|err| {
        SQLError::Internal(format!(
            "MERGE RETURNING projection failed for table `{}` doc {}: {err}",
            input.target_table, input.doc_id
        ))
    })
}

pub(super) fn run_update(
    engine: &Engine,
    stmt: UpdateStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_update_inner(engine, stmt, params))
}

fn run_update_inner(
    engine: &Engine,
    stmt: UpdateStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate the WHERE against each joined row, and apply
    // assignments to the matching target rows. Mirrors the canonical UQA implementation's
    // _compile_update_from.
    if let Some(from_clause) = stmt.from.as_ref() {
        return run_update_from(engine, &stmt, from_clause, params);
    }
    if stmt.with.is_empty() {
        if let Some(result) = try_run_point_update(engine, &stmt, params)? {
            return Ok(result);
        }
    }
    let mut ctes = CteScope::new();
    if !stmt.with.is_empty() {
        materialize_cte_list(engine, &stmt.with, params, &mut ctes)?;
    }
    let scoped_hook = if stmt.with.is_empty() {
        None
    } else {
        Some(ScopedEngineHook::new(engine, &ctes))
    };
    let eval_hook: &dyn uqa_sql::expr::EngineHook = match scoped_hook.as_ref() {
        Some(hook) => hook,
        None => engine,
    };
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    // Without CTEs the WHERE clause resolves through the accelerated
    // single-table machinery (value indexes, posting lists) up front;
    // the per-row re-check below is then unnecessary.
    let preselected = stmt.with.is_empty() && stmt.r#where.is_some();
    let doc_ids: Vec<uqa_core::DocId> = if preselected {
        let filter = stmt.r#where.as_ref().expect("preselected requires WHERE");
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
            if let Some(filter) = stmt.r#where.as_ref() {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(eval_hook);
                if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                    continue;
                }
            }
        }
        for (col, expr) in &stmt.assignments {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(eval_hook);
            let value =
                coerce_to_column_type(engine, &stmt.table, col, uqa_sql::expr::eval(expr, &ctx)?)?;
            doc.insert(col.clone(), value);
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
    stmt: &UpdateStmt,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if !stmt.returning.is_empty() {
        return Ok(None);
    }
    let Some((lookup_field, lookup_value)) =
        point_lookup_filter(stmt.r#where.as_ref(), engine, params)?
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
    filter: Option<&Expr>,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Option<(String, Value)>, SQLError> {
    let Some(Expr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    }) = filter
    else {
        return Ok(None);
    };
    if let Some(field) = top_level_column(lhs) {
        if expr_is_row_independent(rhs) {
            let ctx = EvalContext::new(None, params).with_engine(engine);
            return Ok(Some((field.to_string(), eval(rhs, &ctx)?)));
        }
    }
    if let Some(field) = top_level_column(rhs) {
        if expr_is_row_independent(lhs) {
            let ctx = EvalContext::new(None, params).with_engine(engine);
            return Ok(Some((field.to_string(), eval(lhs, &ctx)?)));
        }
    }
    Ok(None)
}

fn top_level_column(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(name) => Some(name),
        Expr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

fn row_independent_update_values(
    engine: &Engine,
    stmt: &UpdateStmt,
    params: &[SQLParam],
) -> Result<Option<RowIndependentUpdateValues>, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for (column, expr) in &stmt.assignments {
        if !expr_is_row_independent(expr) {
            return Ok(None);
        }
        let value = coerce_to_column_type(engine, &stmt.table, column, eval(expr, &ctx)?)?;
        if let Some(ty) = engine.column_type(&stmt.table, column) {
            if let Ok(values) = index_vectors_for_type(&value, &ty) {
                vectors.insert(column.clone(), values);
            }
        }
        updates.insert(column.clone(), value);
    }
    Ok(Some((updates, vectors)))
}

fn expr_is_row_independent(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_) | Expr::Param(_) => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().all(expr_is_row_independent)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_is_row_independent(lhs) && expr_is_row_independent(rhs)
        }
        Expr::Not(inner) => expr_is_row_independent(inner),
        Expr::IsNull { expr, .. } => expr_is_row_independent(expr),
        Expr::Between { expr, low, high } => {
            expr_is_row_independent(expr)
                && expr_is_row_independent(low)
                && expr_is_row_independent(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_is_row_independent(expr) && list.iter().all(expr_is_row_independent)
        }
        Expr::Case {
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
        Expr::Cast { expr, .. } => expr_is_row_independent(expr),
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Func { .. }
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => false,
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
        let row_ctx = EvalContext::new(Some(document), params).with_engine(engine);
        let result = eval(&expr, &row_ctx)?;
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
                    let ctx = EvalContext::new(Some(old_doc), params).with_engine(engine);
                    eval(&expr, &ctx)?
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
    stmt: &UpdateStmt,
    from_clause: &uqa_sql::ast::FromClause,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut ctes = CteScope::new();
    if !stmt.with.is_empty() {
        materialize_cte_list(engine, &stmt.with, params, &mut ctes)?;
    }
    let from_rows = build_join_rows_with_ctes(engine, from_clause, params, &mut ctes)?;
    let scoped_hook = if stmt.with.is_empty() {
        None
    } else {
        Some(ScopedEngineHook::new(engine, &ctes))
    };
    let eval_hook: &dyn uqa_sql::expr::EngineHook = match scoped_hook.as_ref() {
        Some(hook) => hook,
        None => engine,
    };
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
            if let Some(filter) = stmt.r#where.as_ref() {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&joined), params).with_engine(eval_hook);
                if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                    continue;
                }
            }
            // Apply assignments evaluated against the joined row so
            // RHS expressions can read FROM-side columns.
            let ctx = uqa_sql::expr::EvalContext::new(Some(&joined), params).with_engine(eval_hook);
            for (col, expr) in &stmt.assignments {
                let value =
                    coerce_to_column_type(engine, &target, col, uqa_sql::expr::eval(expr, &ctx)?)?;
                doc.insert(col.clone(), value);
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
    stmt: DeleteStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_delete_inner(engine, stmt, params))
}

fn run_delete_inner(
    engine: &Engine,
    stmt: DeleteStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut to_delete: Vec<uqa_core::DocId> = Vec::new();
    let mut returning_docs: Vec<(uqa_core::DocId, Document)> = Vec::new();
    let mut ctes = CteScope::new();
    if !stmt.with.is_empty() {
        materialize_cte_list(engine, &stmt.with, params, &mut ctes)?;
    }
    // DELETE FROM t USING other WHERE ... -- materialise the join
    // first, then collect target doc ids whose joined image
    // satisfies WHERE. Mirrors the canonical UQA implementation's _compile_delete_using.
    let using_rows: Option<Vec<ResultRow>> = match stmt.using.as_ref() {
        Some(clause) => Some(build_join_rows_with_ctes(
            engine, clause, params, &mut ctes,
        )?),
        None => None,
    };
    let scoped_hook = if stmt.with.is_empty() {
        None
    } else {
        Some(ScopedEngineHook::new(engine, &ctes))
    };
    let eval_hook: &dyn uqa_sql::expr::EngineHook = match scoped_hook.as_ref() {
        Some(hook) => hook,
        None => engine,
    };
    // Plain `DELETE FROM t WHERE ...` resolves the WHERE through the
    // accelerated single-table machinery instead of materialising the
    // whole table.
    let preselected = stmt.with.is_empty() && stmt.using.is_none() && stmt.r#where.is_some();
    let doc_ids: Vec<uqa_core::DocId> = if preselected {
        let filter = stmt.r#where.as_ref().expect("preselected requires WHERE");
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
        let keep = match (stmt.r#where.as_ref(), using_rows.as_ref()) {
            (None, _) => true,
            (Some(_), None) if preselected => true,
            (Some(filter), None) => {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(eval_hook);
                matches!(
                    uqa_sql::expr::eval(filter, &ctx).map(|v| uqa_sql::expr::truthy(&v)),
                    Ok(true)
                )
            }
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
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&joined), params)
                        .with_engine(eval_hook);
                    if matches!(
                        uqa_sql::expr::eval(filter, &ctx).map(|v| uqa_sql::expr::truthy(&v)),
                        Ok(true)
                    ) {
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
                build_returning_row(engine, &stmt.table, doc_id, &doc, &stmt.returning, params)
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
    on_conflict: &uqa_sql::ast::OnConflict,
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
    returning: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut row_doc = document.clone();
    row_doc.insert(DOC_ID_COLUMN.into(), Value::Int(doc_id as i64));
    build_projection_row(Some(engine), &row_doc, returning, params).map_err(|err| {
        SQLError::Internal(format!(
            "RETURNING projection failed for table `{table}` doc {doc_id}: {err}"
        ))
    })
}

fn dml_returning_result(
    engine: &Engine,
    table: &str,
    returning: &[Projection],
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
    stmt: InsertStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_insert_inner(engine, stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_insert_inner(
    engine: &Engine,
    stmt: InsertStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // INSERT ... SELECT: materialise the inner SELECT first, then
    // route each row through the standard add_document path under
    // the named columns.
    if let Some(source) = stmt.select_source.clone() {
        let mut ctes = CteScope::new();
        materialize_cte_list(engine, &stmt.with, params, &mut ctes)?;
        let result = execute_select(engine, &source, params, &mut ctes)?;
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
                document.insert(c.into(), Value::Int(doc_id as i64));
            }
            engine.advance_next_id(&stmt.table, doc_id);
            let document =
                insert_document_with_constraints(engine, &stmt.table, doc_id, document, params)?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &stmt.table,
                    doc_id,
                    &document,
                    &stmt.returning,
                    params,
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
    let ctx = EvalContext::new(None, params).with_engine(engine);
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
            let mut v = eval(&row[i], &ctx)?;
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
                let v =
                    coerce_to_column_type(engine, &stmt.table, &col, eval(&default_expr, &ctx)?)?;
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
            let row_ctx = EvalContext::new(Some(&document), params).with_engine(engine);
            let result = eval(&expr, &row_ctx)?;
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
                    uqa_sql::ast::OnConflictAction::Nothing => {
                        continue;
                    }
                    uqa_sql::ast::OnConflictAction::Update {
                        assignments,
                        r#where,
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
                        let row_ctx =
                            EvalContext::new(Some(&conflict_ctx_doc), params).with_engine(engine);
                        if let Some(pred) = r#where {
                            let keep = eval(pred, &row_ctx)?;
                            if !uqa_sql::expr::truthy(&keep) {
                                continue;
                            }
                        }
                        let mut updated_doc = existing_doc.clone();
                        for (col, expr) in assignments {
                            let v = coerce_to_column_type(
                                engine,
                                &stmt.table,
                                col,
                                eval(expr, &row_ctx)?,
                            )?;
                            updated_doc.insert(col.clone(), v.clone());
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
                            )?);
                        }
                        affected += 1;
                        continue;
                    }
                }
            }
        }

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
        let document =
            insert_document_with_constraints(engine, &stmt.table, doc_id, document, params)?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                doc_id,
                &document,
                &stmt.returning,
                params,
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

fn insert_document_with_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    mut document: Document,
    params: &[SQLParam],
) -> Result<Document, SQLError> {
    apply_missing_column_defaults(engine, table, &mut document, params)?;
    validate_document_constraints(engine, table, doc_id, &document, params)?;
    engine.add_document_with_vector_values(
        table,
        doc_id,
        document.clone(),
        document_vectors(engine, table, &document),
    )?;
    Ok(document)
}

fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for col in engine.table_columns(table) {
        if document.contains_key(&col) {
            continue;
        }
        if let Some(default_expr) = engine.column_default_expr(table, &col) {
            let value = coerce_to_column_type(engine, table, &col, eval(&default_expr, &ctx)?)?;
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
