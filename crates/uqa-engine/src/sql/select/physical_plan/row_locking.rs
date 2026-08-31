//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple-lock recheck operator construction.

use std::sync::Arc;

use uqa_execution::{PhysicalOperator, Project};
use uqa_planner::QueryBlockPlan;
use uqa_sql::{SQLError, SQLParam};

use super::super::{
    column_prune_for_stmt, final_filter_after_qualifier_pushdown,
    prepare_correlated_exists_predicate, qualifier_filters_for_stmt, CteScope, Engine,
    EngineExpressionEvaluator, PhysicalProjection,
};

/// Rebuild the plan below one `LockRows` boundary for a tuple-local recheck. The construction replays the same source, filter, and scalar target projection below the original `LockRows` boundary, with the recheck pins active in `ctes` so every lock-target base scan emits only the candidate's tuples while unmarked relations rescan under the statement snapshot. Sorting, locking, and `LIMIT` never run here: the candidate keeps its original position in the outer stream.
#[cold]
#[inline(never)]
pub(in crate::sql) fn build_row_lock_recheck_operator<'a>(
    engine: &'a Engine,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    _ordered: bool,
    projections: &[PhysicalProjection],
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(from) = statement.from.as_ref() else {
        return Err(SQLError::Internal(
            "row-lock recheck requires a FROM clause".into(),
        ));
    };
    let column_prune = column_prune_for_stmt(engine, statement, from, ctes)?;
    let qualifier_filters = qualifier_filters_for_stmt(engine, statement, from, ctes)?;
    let source_row_locks = crate::sql::select::resolve_row_locks(
        engine,
        from,
        &statement.locking,
        statement.r#where.as_ref(),
        params,
        ctes,
    )?;
    let mut operator = {
        let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
        crate::sql::from_rows::build_join_operator_with_recheck_pins(
            engine,
            from,
            params,
            &mut scoped_ctes,
            column_prune.as_ref(),
            qualifier_filters.as_ref(),
        )?
    };
    if let Some(outer_row) = ctes.row_lock_outer_row() {
        operator = Box::new(uqa_execution::ScopeOverlay::new(
            operator,
            outer_row.clone(),
        ));
    }
    let predicate = final_filter_after_qualifier_pushdown(
        engine,
        statement,
        from,
        qualifier_filters.as_ref(),
        ctes,
    )?;
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    if let Some(predicate) = predicate {
        operator = match prepare_correlated_exists_predicate(engine, &predicate, params, ctes)? {
            Some(prepared) => Box::new(uqa_execution::Filter::with_row_predicate(
                operator, prepared,
            )),
            None => Box::new(uqa_execution::Filter::with_evaluator(
                operator,
                predicate,
                Arc::clone(&evaluator),
            )),
        };
    }
    operator = if projections.is_empty() {
        operator
    } else {
        Box::new(Project::appending_target_evaluator(
            operator,
            projections.to_vec(),
            evaluator,
        )) as Box<dyn PhysicalOperator + 'a>
    };
    Ok(operator)
}
