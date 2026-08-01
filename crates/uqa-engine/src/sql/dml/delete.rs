//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE execution and referenced-key delete actions.

use super::{
    apply_set_action_to_child, build_join_spill_with_ctes, build_returning_row,
    dml_returning_result, eval_mutation_expr, missing_document_error, referencing_rows,
    referrers_to_for_actions, rewrite_document_with_referential_actions, BTreeSet, CteScope,
    DeletePlan, DocId, Document, Engine, ForeignKey, ForeignKeyAction, ResultRow, SQLError,
    SQLParam, SQLResult, Value,
};

pub(in crate::sql) fn run_delete(
    engine: &Engine,
    stmt: DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_delete_inner(engine, &stmt, params))
}

pub(in crate::sql) fn run_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut to_delete: Vec<uqa_core::DocId> = Vec::new();
    let mut returning_docs: Vec<(uqa_core::DocId, Document)> = Vec::new();
    let mut ctes = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
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
        crate::sql::where_eval::collect_where_doc_ids(engine, &stmt.table, filter, params)?
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
                    .map_err(crate::sql::select::physical_exec_error)?;
                for using_row in reader {
                    let using_row = using_row.map_err(crate::sql::select::physical_exec_error)?;
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

pub(in crate::sql) fn delete_document_with_referential_actions(
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

pub(in crate::sql) fn apply_referenced_key_delete_actions(
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

pub(in crate::sql) fn delete_set_columns(fk: &ForeignKey) -> Vec<String> {
    if fk.on_delete_set_columns.is_empty() {
        fk.local_columns.clone()
    } else {
        fk.on_delete_set_columns.clone()
    }
}
