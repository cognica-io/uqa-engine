//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE FROM join-source execution.

use super::{
    build_join_spill_with_ctes, build_returning_row, dml_returning_result,
    eval_mutation_assignment, eval_mutation_expr, missing_document_error,
    rewrite_document_with_referential_actions, CteScope, Engine, MutationAssignmentTarget,
    ResultRow, ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult, SourcePlan,
    UpdatePlan,
};

pub(in crate::sql) fn run_update_from(
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
            .map_err(crate::sql::select::physical_exec_error)?;
        for from_row in from_reader {
            let from_row = from_row.map_err(crate::sql::select::physical_exec_error)?;
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
            for (k, v) in from_row.view().iter() {
                joined.insert(k.to_string(), v.clone());
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
            let rewritten_doc_id = rewrite_document_with_referential_actions(
                engine,
                &target,
                doc_id,
                &original_doc,
                &mut doc,
                params,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &target,
                    ReturningRowImages {
                        old: Some(ReturningRowImage {
                            doc_id,
                            document: &original_doc,
                        }),
                        new: Some(ReturningRowImage {
                            doc_id: rewritten_doc_id,
                            document: &doc,
                        }),
                    },
                    &stmt.returning_aliases,
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
