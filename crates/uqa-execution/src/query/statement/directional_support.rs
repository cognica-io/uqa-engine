//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backward traversal support derived from the physical plan and routine metadata.
use crate::BackwardScanSupport;
use uqa_sql::{
    ast::SetOpKind,
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    routines::RoutineResolution,
    semantics::sets::static_setness::function_may_return_set_statically,
};
pub fn query_plan_backward_scan_support(
    catalog: &dyn RoutineResolution,
    plan: &QueryPlan,
) -> BackwardScanSupport {
    match &plan.root {
        RelationalPlan::Values { .. } => BackwardScanSupport::Native,
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            ..
        } if matches!((*kind, *all), (SetOpKind::Union, true)) && order_by.is_empty() => {
            if query_plan_backward_scan_support(catalog, left) == BackwardScanSupport::Native
                && query_plan_backward_scan_support(catalog, right) == BackwardScanSupport::Native
            {
                BackwardScanSupport::Native
            } else {
                BackwardScanSupport::Unsupported
            }
        }
        RelationalPlan::SetOp { .. } => BackwardScanSupport::Unsupported,
        RelationalPlan::QueryBlock(block) => query_block_backward_scan_support(catalog, block),
    }
}

fn query_block_backward_scan_support(
    catalog: &dyn RoutineResolution,
    block: &QueryBlockPlan,
) -> BackwardScanSupport {
    if block.distinct
        || !block.distinct_on.is_empty()
        || !block.locking.is_empty()
        || matches!(block.compute, ComputePlan::Window)
        || projections_may_return_set_statically(catalog, block)
    {
        return BackwardScanSupport::Unsupported;
    }
    if matches!(block.compute, ComputePlan::Aggregate) {
        return if block.order_by.is_empty() {
            BackwardScanSupport::Unsupported
        } else {
            BackwardScanSupport::Native
        };
    }
    let Some(source) = block.from.as_ref() else {
        return BackwardScanSupport::Unsupported;
    };
    if !block.order_by.is_empty() {
        return BackwardScanSupport::Native;
    }
    source_backward_scan_support(catalog, source)
}

fn source_backward_scan_support(
    catalog: &dyn RoutineResolution,
    source: &SourcePlan,
) -> BackwardScanSupport {
    match source {
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => BackwardScanSupport::Native,
        SourcePlan::Subquery { body, .. } => query_plan_backward_scan_support(catalog, body),
        SourcePlan::Join { .. } => BackwardScanSupport::Unsupported,
    }
}

fn projections_may_return_set_statically(
    catalog: &dyn RoutineResolution,
    block: &QueryBlockPlan,
) -> bool {
    block.projections.iter().any(|projection| {
        let mut returns_set = false;
        projection.expr.visit(&mut |expression| {
            let uqa_sql::ScalarExpr::Func { name, binding, .. } = expression else {
                return;
            };
            returns_set |= function_may_return_set_statically(catalog, name, binding.as_ref());
        });
        returns_set
    })
}
