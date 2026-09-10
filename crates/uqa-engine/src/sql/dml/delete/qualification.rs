//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE rule qualification dependencies and source cardinality.

use super::super::{
    eval_mutation_expr, BTreeSet, CteScope, DeletePlan, Engine, SQLError, SQLParam, DOC_ID_COLUMN,
    TABLE_OID_COLUMN, XMIN_COLUMN,
};

pub(super) fn delete_qualification_references_target(
    engine: &Engine,
    stmt: &DeletePlan,
    predicate: Option<&uqa_execution::ScalarExpr>,
) -> Result<bool, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(false);
    };
    if crate::sql::select::expr_contains_subquery(predicate) {
        return Ok(true);
    }
    let qualifiers = crate::sql::select::expr_qualifiers(predicate);
    if qualifiers.iter().any(|qualifier| {
        qualifier.eq_ignore_ascii_case(&stmt.target_qualifier)
            || qualifier.eq_ignore_ascii_case(&stmt.table)
    }) {
        return Ok(true);
    }
    if !crate::sql::select::expr_has_unqualified_column(predicate) {
        return Ok(false);
    }
    let mut columns = BTreeSet::new();
    if !predicate.collect_columns(&mut columns) {
        return Ok(true);
    }
    let target_columns = engine
        .try_query_table_columns(&stmt.table)
        .map_err(|error| SQLError::Internal(format!("read DELETE target columns: {error}")))?
        .into_iter()
        .chain([
            DOC_ID_COLUMN.to_string(),
            TABLE_OID_COLUMN.to_string(),
            XMIN_COLUMN.to_string(),
        ])
        .collect::<BTreeSet<_>>();
    Ok(!columns.is_disjoint(&target_columns))
}

pub(super) fn count_delete_source_qualifications(
    engine: &Engine,
    stmt: &DeletePlan,
    ctes: &CteScope,
    using_rows: &uqa_execution::SharedSpill,
    params: &[SQLParam],
) -> Result<usize, SQLError> {
    let mut count = 0;
    for source in using_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?
    {
        let source = source.map_err(crate::sql::select::physical_exec_error)?;
        let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(&source), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        count += usize::from(qualifies);
    }
    Ok(count)
}
