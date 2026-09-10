//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture authorization analysis inputs from the statement scope.
use super::CteScope;
use std::collections::BTreeSet;
use uqa_sql::semantics::privileges::{
    self as logical,
    context::{PrivilegeCteCatalog, PrivilegeScope},
};
use uqa_sql::{
    plan::{CtePlan, QueryBlockPlan, QueryPlan, SourcePlan},
    SQLError, ScalarExpr,
};
impl<S: Clone> PrivilegeCteCatalog for CteScope<S> {
    fn is_visible_cte(&self, name: &str) -> bool {
        self.is_visible_cte(name)
    }
    fn materialized_columns(&self, name: &str) -> Option<Vec<String>> {
        self.materialized_for_scan(name)
            .map(|rows| rows.row_schema().columns().to_vec())
    }
    fn deferred_reference(&self, name: &str) -> Option<&CtePlan> {
        self.deferred_reference(name)
    }
    fn privilege_subject(&self) -> Result<&str, SQLError> {
        self.privilege_subject()
    }
}
fn with_scope<S: Clone, T>(
    ctes: &CteScope<S>,
    apply: impl FnOnce(&PrivilegeScope<'_>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let scope = PrivilegeScope::new(
        &catalog,
        ctes.relation_name_resolution()?,
        ctes,
        ctes.scalar_subqueries.clone(),
    );
    apply(&scope)
}
pub fn ensure_select_privileges_for_query_block<S: Clone>(
    statement: &QueryBlockPlan,
    source: &SourcePlan,
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    with_scope(ctes, |scope| {
        logical::ensure_select_privileges_for_query_block(statement, source, scope)
    })
}

pub fn ensure_select_privileges_for_source_expressions<S: Clone>(
    source: &SourcePlan,
    expressions: &[&ScalarExpr],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    with_scope(ctes, |scope| {
        logical::ensure_select_privileges_for_source_expressions(source, expressions, scope)
    })
}

pub fn ensure_select_privileges_for_table_expressions<S: Clone>(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&ScalarExpr],
    subqueries: &[QueryPlan],
    required_columns: &[String],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    with_scope(ctes, |scope| {
        logical::ensure_select_privileges_for_table_expressions(
            table,
            qualifiers,
            expressions,
            subqueries,
            required_columns,
            scope,
        )
    })
}

use uqa_sql::semantics::privileges::TargetSelectPrivilegeRequest;
pub fn ensure_target_table_select_for_expressions<S: Clone>(
    request: TargetSelectPrivilegeRequest<'_, '_>,
    ctes: &mut CteScope<S>,
) -> Result<(), SQLError> {
    let TargetSelectPrivilegeRequest {
        table,
        privilege_subject,
        target_qualifier,
        returning_aliases,
        expressions,
        subqueries,
        required_columns,
    } = request;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let target_qualifiers = BTreeSet::from([
        target_qualifier.to_string(),
        table.to_string(),
        relation.name,
        returning_aliases.old.clone(),
        returning_aliases.new.clone(),
    ]);
    ctes.scalar_subqueries = subqueries.to_vec();
    if let Some(subject) = privilege_subject {
        let scope = ctes.enter_privilege_subject(subject.to_string());
        ensure_select_privileges_for_table_expressions(
            table,
            &target_qualifiers,
            expressions,
            subqueries,
            required_columns,
            &scope,
        )
    } else {
        ensure_select_privileges_for_table_expressions(
            table,
            &target_qualifiers,
            expressions,
            subqueries,
            required_columns,
            ctes,
        )
    }
}
