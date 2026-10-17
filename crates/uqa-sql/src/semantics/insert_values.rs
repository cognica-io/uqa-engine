//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single `INSERT ... VALUES` row, which `PostgreSQL` processes like the target list of a SELECT without FROM.

use super::view_rewrite::context::ViewRewriteContext;
use crate::ast::AssignmentTarget;
use crate::catalog::view::StoredViewKind;
use crate::plan::{
    AccessPathPlan, ComputePlan, InsertPlan, ProjectionPlan, QueryBlockPlan, QueryPlan,
    RelationalPlan,
};
use crate::{SQLError, ScalarExpr};
use std::collections::BTreeSet;

/// The `INSERT ... SELECT` form of an INSERT whose single VALUES row contains a set-returning call, so the call expands into one inserted row per result as in `PostgreSQL`. A DEFAULT entry leaves its column out of the target list, where the column default fills it as for an omitted column. Any other INSERT has no such form.
///
/// `observed` names the NEW columns a rewrite rule of the target reads, or `None` when a rule reads the whole row; it is consulted only for a row that contains a set-returning call. `PostgreSQL` substitutes each VALUES entry for the NEW references to its column, so a rule action or condition that reads a set-returning entry evaluates it outside any target list and fails. `implicit_columns` names the target's columns when the INSERT lists none.
pub fn single_row_values_query(
    routines: &dyn crate::routines::RoutineResolution,
    insert: &InsertPlan,
    observed: impl FnOnce() -> Result<Option<BTreeSet<String>>, SQLError>,
    implicit_columns: impl FnOnce() -> Result<Option<Vec<String>>, SQLError>,
) -> Result<Option<InsertPlan>, SQLError> {
    let [row] = insert.rows.as_slice() else {
        return Ok(None);
    };
    let set_valued = row
        .iter()
        .map(|value| {
            crate::semantics::sets::static_setness::expression_may_return_set_statically(
                routines, value,
            )
        })
        .collect::<Vec<_>>();
    if !set_valued.contains(&true) {
        return Ok(None);
    }
    let Some(observed) = observed()? else {
        return Err(crate::semantics::sets::placement::set_valued_call_error());
    };
    let defaults = row.iter().any(|value| matches!(value, ScalarExpr::Default));
    let targets = if !insert.columns.is_empty() {
        insert.columns.clone()
    } else if defaults || !observed.is_empty() {
        let Some(columns) = implicit_columns()? else {
            return Ok(None);
        };
        columns
            .into_iter()
            .take(row.len())
            .map(AssignmentTarget::from)
            .collect()
    } else {
        Vec::new()
    };
    let explicit = !insert.columns.is_empty() || defaults;
    if (explicit || !targets.is_empty()) && targets.len() != row.len() {
        return Ok(None);
    }
    if targets
        .iter()
        .zip(&set_valued)
        .any(|(target, set_valued)| *set_valued && observed.contains(&target.column))
    {
        return Err(crate::semantics::sets::placement::set_valued_call_error());
    }
    let mut columns = Vec::with_capacity(targets.len());
    let mut projections = Vec::with_capacity(row.len());
    for (position, value) in row.iter().enumerate() {
        if matches!(value, ScalarExpr::Default) {
            continue;
        }
        if explicit {
            columns.push(targets[position].clone());
        }
        projections.push(ProjectionPlan {
            expr: value.clone(),
            alias: None,
        });
    }
    let source = QueryPlan {
        relations_bound: insert.relations_bound,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            windows: Vec::new(),
            projections,
            privilege_columns: std::collections::BTreeSet::default(),
            from: None,
            r#where: None,
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: insert.subqueries.clone(),
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    };
    let mut expanded = insert.clone();
    expanded.rows = Vec::new();
    expanded.columns = columns;
    expanded.source = Some(Box::new(source));
    Ok(Some(expanded))
}

/// The columns an INSERT without a column list fills in order: a table's columns or a view's output columns. A materialized view cannot be a target, and its INSERT keeps the form that reports so.
pub fn implicit_target_columns(
    context: ViewRewriteContext<'_>,
    table: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    match context.catalog.target_view_kind(table)? {
        Some(StoredViewKind::View) => Ok(Some(
            super::view_mutation::resolve_view_target(context, table)?.columns,
        )),
        Some(StoredViewKind::Materialized) => Ok(None),
        None => context
            .catalog
            .try_table_columns(table)
            .map(Some)
            .map_err(SQLError::Internal),
    }
}

#[cfg(test)]
mod tests;
