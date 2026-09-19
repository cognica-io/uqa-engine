//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backward traversal support derived from query operators and CTE scheduling.

use crate::{
    query::{cte::strategy::schedule_plan_ctes, CteScope},
    BackwardScanSupport,
};
use std::{borrow::Cow, collections::BTreeMap};
use uqa_sql::{
    ast::SetOpKind,
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    routines::RoutineResolution,
    semantics::{cte_reference_name, volatility::VolatilityCatalog},
    SQLError, SQLParam,
};

mod ordering;
use ordering::{
    has_effective_ordering, ordered_set_projection, projections_return_set, window_preserves_order,
};

pub fn query_plan_backward_scan_support<S: Clone>(
    routines: &dyn RoutineResolution,
    volatility: &dyn VolatilityCatalog,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<BackwardScanSupport, SQLError> {
    TraversalAnalysis {
        routines,
        volatility,
        params,
    }
    .query(plan, ctes, &BTreeMap::new())
}

struct TraversalAnalysis<'a> {
    routines: &'a dyn RoutineResolution,
    volatility: &'a dyn VolatilityCatalog,
    params: &'a [SQLParam],
}

impl TraversalAnalysis<'_> {
    fn query<S: Clone>(
        &self,
        plan: &QueryPlan,
        ctes: &CteScope<S>,
        inherited: &BTreeMap<String, BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        let mut ctes = Cow::Borrowed(ctes);
        let mut support = Cow::Borrowed(inherited);
        for scheduled in schedule_plan_ctes(self.volatility, plan)? {
            let cte = scheduled.plan;
            let traversal = if scheduled.deferred {
                let query = cte
                    .body
                    .query()
                    .ok_or_else(|| SQLError::Internal("deferred CTE has no query body".into()))?;
                self.query(query, &ctes, &support)?
            } else {
                BackwardScanSupport::Native
            };
            support.to_mut().insert(cte.name.clone(), traversal);
            // Retain declaration-order schemas without executing or materializing a CTE.
            ctes.to_mut().insert_deferred(cte.clone());
        }
        match &plan.root {
            RelationalPlan::Values { .. } => Ok(BackwardScanSupport::Native),
            RelationalPlan::SetOp {
                kind: SetOpKind::Union,
                all: true,
                left,
                right,
                order_by,
                ..
            } => {
                if !order_by.is_empty()
                    || (self.query(left, &ctes, &support)? == BackwardScanSupport::Native
                        && self.query(right, &ctes, &support)? == BackwardScanSupport::Native)
                {
                    Ok(BackwardScanSupport::Native)
                } else {
                    Ok(BackwardScanSupport::Unsupported)
                }
            }
            RelationalPlan::SetOp { .. } => Ok(BackwardScanSupport::Unsupported),
            RelationalPlan::QueryBlock(block) => self.block(block, &ctes, &support),
        }
    }

    fn block<S: Clone>(
        &self,
        block: &QueryBlockPlan,
        ctes: &CteScope<S>,
        support: &BTreeMap<String, BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        if block.distinct || !block.distinct_on.is_empty() || !block.locking.is_empty() {
            return Ok(BackwardScanSupport::Unsupported);
        }
        let returns_set = projections_return_set(self.routines, block);
        if !block.order_by.is_empty()
            && has_effective_ordering(self.routines, block, self.params, ctes)?
        {
            if returns_set && !ordered_set_projection(self.routines, block, self.params, ctes)? {
                return Ok(BackwardScanSupport::Unsupported);
            }
            if matches!(block.compute, ComputePlan::Window)
                && window_preserves_order(self.routines, block, self.params, ctes)?
            {
                return Ok(BackwardScanSupport::Unsupported);
            }
            if returns_set || !matches!(block.compute, ComputePlan::Project) || block.from.is_some()
            {
                return Ok(BackwardScanSupport::Native);
            }
        }
        if returns_set || !matches!(block.compute, ComputePlan::Project) {
            return Ok(BackwardScanSupport::Unsupported);
        }
        match &block.from {
            Some(source) => self.source(source, ctes, support),
            None => Ok(BackwardScanSupport::Unsupported),
        }
    }

    fn source<S: Clone>(
        &self,
        source: &SourcePlan,
        ctes: &CteScope<S>,
        support: &BTreeMap<String, BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        match source {
            SourcePlan::Table { name, .. } => {
                if let Some(traversal) =
                    cte_reference_name(name).and_then(|name| support.get(&name))
                {
                    return Ok(*traversal);
                }
                if let Some(deferred) = ctes.deferred_reference(name) {
                    let query = deferred.body.query().ok_or_else(|| {
                        SQLError::Internal("deferred CTE has no query body".into())
                    })?;
                    let mut parent = ctes.clone();
                    parent.remove_deferred(&deferred.name);
                    return self.query(query, &parent, support);
                }
                Ok(BackwardScanSupport::Native)
            }
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. } => Ok(BackwardScanSupport::Native),
            SourcePlan::Subquery { body, .. } => self.query(body, ctes, support),
            SourcePlan::Join { .. } => Ok(BackwardScanSupport::Unsupported),
        }
    }
}
