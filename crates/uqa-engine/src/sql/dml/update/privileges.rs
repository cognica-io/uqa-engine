//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-level target and source privilege analysis for UPDATE.

use super::super::{CteScope, Engine, SQLError, ScalarExpr, UpdatePlan};

pub(super) fn ensure_update_target_privileges<'a>(
    engine: &Engine,
    statement: &'a UpdatePlan,
) -> Result<Vec<&'a ScalarExpr>, SQLError> {
    for assignment in &statement.assignments {
        engine.ensure_column_privilege(
            &statement.table,
            &assignment.column,
            crate::engine_table_security::TableAclPrivilege::Update,
        )?;
    }
    let expressions = statement
        .assignments
        .iter()
        .map(|assignment| &assignment.value)
        .chain(statement.predicate.iter())
        .chain(
            statement
                .returning
                .iter()
                .map(|projection| &projection.expr),
        )
        .collect::<Vec<_>>();
    super::super::ensure_target_table_select_for_expressions(
        engine,
        &statement.table,
        &statement.target_qualifier,
        &statement.returning_aliases,
        &expressions,
        &statement.subqueries,
        &[],
    )?;
    Ok(expressions)
}

pub(super) fn ensure_update_source_privileges(
    statement: &UpdatePlan,
    expressions: &[&ScalarExpr],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let Some(source) = statement.source.as_deref() else {
        return Ok(());
    };
    crate::sql::select::ensure_select_privileges_for_source_expressions(source, expressions, ctes)
}
