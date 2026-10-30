//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single `INSERT ... VALUES` row, which `PostgreSQL` processes like the target list of a SELECT without FROM.

use super::view_rewrite::context::ViewRewriteContext;
use crate::catalog::view::StoredViewKind;
use crate::plan::{CommandPlan, InsertPlan};
use crate::SQLError;
use std::collections::BTreeSet;
use uqa_core::memory::ProductionControl;

mod production;
pub use production::{implicit_target_columns_with_control, single_row_values_query_with_control};

/// The `INSERT ... SELECT` form of an INSERT whose single VALUES row contains a set-returning call, so the call expands into one inserted row per result as in `PostgreSQL`. A DEFAULT entry leaves its column out of the target list, where the column default fills it as for an omitted column. Any other INSERT has no such form.
///
/// `observed` names the NEW columns a rewrite rule of the target reads, or `None` when a rule reads the whole row; it is consulted only for a row that contains a set-returning call. `PostgreSQL` substitutes each VALUES entry for the NEW references to its column, so a rule action or condition that reads a set-returning entry evaluates it outside any target list and fails. `implicit_columns` names the target's columns when the INSERT lists none.
pub fn single_row_values_query(
    routines: &dyn crate::routines::RoutineResolution,
    insert: &InsertPlan,
    observed: impl FnOnce() -> Result<Option<BTreeSet<String>>, SQLError>,
    implicit_columns: impl FnOnce() -> Result<Option<Vec<String>>, SQLError>,
) -> Result<Option<InsertPlan>, SQLError> {
    let control = ProductionControl::uncontrolled();
    let expanded = single_row_values_query_with_control(
        routines,
        insert,
        || {
            observed()?
                .map(|names| Ok(control.finish(names.into_iter().collect(), None)?))
                .transpose()
        },
        || {
            implicit_columns()?
                .map(|names| Ok(control.finish(names, None)?))
                .transpose()
        },
        &control,
    )?;
    expanded
        .map(|expanded| {
            let CommandPlan::Insert(plan) = expanded
                .finish(&control)?
                .into_uncontrolled()
                .expect("ordinary VALUES expansion")
            else {
                unreachable!("INSERT VALUES expansion")
            };
            Ok(*plan)
        })
        .transpose()
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
