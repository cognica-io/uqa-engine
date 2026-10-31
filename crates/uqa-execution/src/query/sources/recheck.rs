//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebuild physical source and scalar projection below a tuple-lock boundary.

use super::{CteScope, SourceContext};
use crate::query::PhysicalProjection;
use crate::{PhysicalOperator, Project};
use std::sync::Arc;
use uqa_sql::{plan::QueryBlockPlan, SQLError, SQLParam};

/// Rebuild the plan below one `LockRows` boundary for a tuple-local recheck. The construction replays the same source, filter, and scalar target projection below the original `LockRows` boundary, with the recheck pins active in `ctes` so every lock-target base scan emits only the candidate's tuples while unmarked relations rescan under the statement snapshot. Sorting, locking, and `LIMIT` never run here: the candidate keeps its original position in the outer stream.
#[cold]
#[inline(never)]
pub fn build_row_lock_recheck_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    _ordered: bool,
    projections: &[PhysicalProjection],
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(from) = statement.from.as_ref() else {
        return Err(SQLError::Internal(
            "row-lock recheck requires a FROM clause".into(),
        ));
    };
    let source_schema = crate::query::binding::bind_source_plan_schema(
        context.ctes.routines,
        from,
        params,
        ctes,
        ctes.row_lock_outer_row().map(|row| &row.schema),
    )?;
    let column_prune =
        context
            .planning
            .column_prune(statement, from, ctes, Some(&source_schema))?;
    let qualifier_filters = context.planning.qualifier_filters(statement, from, ctes)?;
    let source_row_locks = crate::query::locking::resolve_row_locks(
        context.locking,
        from,
        &statement.locking,
        statement.r#where.as_ref(),
        params,
        ctes,
    )?;
    let mut operator = {
        let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
        super::build_join_operator_with_recheck_pins(
            context,
            from,
            params,
            &mut scoped_ctes,
            column_prune.as_ref(),
            qualifier_filters.as_ref(),
        )?
    };
    if let Some(outer_row) = ctes.row_lock_outer_row() {
        operator = Box::new(crate::ScopeOverlay::new(operator, outer_row.clone()));
    }
    crate::physical::construction::OperatorBuild::build(operator, |operator| {
        let predicate =
            context
                .planning
                .residual_filter(statement, from, qualifier_filters.as_ref(), ctes)?;
        let evaluator = context.relational.evaluator(params, ctes)?;
        if let Some(predicate) = predicate {
            let prepared = context
                .relational
                .expressions
                .prepare_predicate(&predicate, params, ctes)?;
            operator.wrap(|input| match prepared {
                Some(prepared) => Box::new(crate::Filter::with_row_predicate(input, prepared)),
                None => Box::new(crate::Filter::with_evaluator(
                    input,
                    predicate,
                    Arc::clone(&evaluator),
                )),
            });
        }
        if !projections.is_empty() {
            let work_mem = context.relational.runtime.work_mem_bytes()?;
            operator.wrap(|input| {
                Box::new(
                    Project::appending_target_evaluator(input, projections.to_vec(), evaluator)
                        .with_memory_budget(work_mem),
                )
            });
        }
        Ok(())
    })
}
