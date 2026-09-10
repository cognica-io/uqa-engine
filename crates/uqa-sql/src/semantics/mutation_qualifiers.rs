//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation qualifier visibility in mutation expressions.
use crate::{SQLError, ScalarExpr};
use std::collections::BTreeSet;

pub fn validate_dml_expression_qualifiers(
    expression: &ScalarExpr,
    allowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for qualifier in super::expr_qualifiers(expression) {
        if !allowed.contains(&qualifier) {
            return Err(SQLError::UnknownTable(qualifier));
        }
    }
    Ok(())
}

/// Visible target column names, including schema-less relation columns.
pub trait MutationTargetColumns {
    fn try_query_table_columns(&self, table: &str) -> Result<Vec<String>, String>;
}
pub fn qualification_references_target(
    catalog: &dyn MutationTargetColumns,
    table: &str,
    target_qualifier: &str,
    operation: &str,
    predicate: Option<&ScalarExpr>,
) -> Result<bool, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(false);
    };
    if super::expr_contains_subquery(predicate) {
        return Ok(true);
    }
    let qualifiers = super::expr_qualifiers(predicate);
    if qualifiers.iter().any(|qualifier| {
        qualifier.eq_ignore_ascii_case(target_qualifier) || qualifier.eq_ignore_ascii_case(table)
    }) {
        return Ok(true);
    }
    if !super::expr_has_unqualified_column(predicate) {
        return Ok(false);
    }
    let mut columns = std::collections::BTreeSet::new();
    if !predicate.collect_columns(&mut columns) {
        return Ok(true);
    }
    let target_columns = catalog
        .try_query_table_columns(table)
        .map_err(|error| SQLError::Internal(format!("read {operation} target columns: {error}")))?
        .into_iter()
        .chain([
            super::DOC_ID_COLUMN.to_string(),
            super::TABLE_OID_COLUMN.to_string(),
            super::XMIN_COLUMN.to_string(),
        ])
        .collect::<std::collections::BTreeSet<_>>();
    Ok(!columns.is_disjoint(&target_columns))
}
