//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE execution, point-update fast paths, and patch eligibility.

use super::{
    build_returning_row, coerce_to_column_type, dml_returning_result, dml_storage_error,
    eval_mutation_expr, index_vectors_for_type, missing_document_error, referrers_to_for_actions,
    rewrite_document_with_referential_actions, run_update_from, validate_mutation_columns,
    BTreeMap, BTreeSet, BinaryOp, ColumnType, CteScope, Engine, RowIndependentUpdateValues,
    SQLError, SQLParam, SQLResult, ScalarExpr, UpdatePlan, Value,
};

pub(in crate::sql) fn run_update(
    engine: &Engine,
    stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_update_inner(engine, &stmt, params))
}

pub(in crate::sql) fn run_update_inner(
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
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);

    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate WHERE against each joined row, and apply assignments to the
    // matching target rows.
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
        crate::sql::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
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

pub(in crate::sql) fn try_run_point_update(
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

pub(in crate::sql) fn point_lookup_filter(
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

pub(in crate::sql) fn top_level_column(expr: &ScalarExpr) -> Option<&str> {
    match expr {
        ScalarExpr::Column(name) => Some(name),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

pub(in crate::sql) fn row_independent_update_values(
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

pub(in crate::sql) fn expr_is_row_independent(expr: &ScalarExpr) -> bool {
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

pub(in crate::sql) fn can_patch_update_without_full_row(
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

pub(in crate::sql) fn point_lookup_field_is_unique(
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
