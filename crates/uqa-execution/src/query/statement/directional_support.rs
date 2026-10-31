//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backward traversal support derived from query operators and CTE scheduling.

use crate::{query::CteScope, BackwardScanSupport};
use uqa_sql::binding::production::BindingMap;
use uqa_sql::{
    ast::SetOpKind,
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    routines::RoutineResolution,
    semantics::volatility::VolatilityCatalog,
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
    control: uqa_core::memory::ProductionControl<'_>,
) -> Result<BackwardScanSupport, SQLError> {
    TraversalAnalysis {
        routines,
        volatility,
        params,
        control,
    }
    .query(plan, ctes, &BindingMap::new())
}

struct TraversalAnalysis<'a> {
    routines: &'a dyn RoutineResolution,
    volatility: &'a dyn VolatilityCatalog,
    params: &'a [SQLParam],
    control: uqa_core::memory::ProductionControl<'a>,
}

impl TraversalAnalysis<'_> {
    fn query<S: Clone>(
        &self,
        plan: &QueryPlan,
        ctes: &CteScope<S>,
        inherited: &BindingMap<BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        let mut owned_ctes: Option<CteScope<S>> = None;
        let mut owned_support: Option<BindingMap<BackwardScanSupport>> = None;
        let scheduled = uqa_sql::semantics::cte_strategy::schedule_plan_ctes_with_control(
            self.volatility,
            plan,
            self.control,
        )?;
        for scheduled in scheduled.iter() {
            let cte = scheduled.plan;
            let traversal = if scheduled.deferred {
                let query = cte
                    .body
                    .query()
                    .ok_or_else(|| SQLError::Internal("deferred CTE has no query body".into()))?;
                self.query(
                    query,
                    owned_ctes.as_ref().unwrap_or(ctes),
                    owned_support.as_ref().unwrap_or(inherited),
                )?
            } else {
                BackwardScanSupport::Native
            };
            if owned_support.is_none() {
                owned_support =
                    Some(inherited.copy_with_control(&self.control, |value| Ok(*value))?);
            }
            owned_support
                .as_mut()
                .expect("private traversal map")
                .insert_with_control(&cte.name, traversal, &self.control)?;
            // Retain declaration-order schemas without executing or materializing a CTE.
            if owned_ctes.is_none() {
                owned_ctes = Some(ctes.copy_with_control(&self.control)?);
            }
            owned_ctes
                .as_mut()
                .expect("private declaration scope")
                .insert_deferred_with_control(
                    uqa_sql::plan::RetainedCtePlan::capture(cte, &self.control)?,
                    &self.control,
                )?;
        }
        let ctes = owned_ctes.as_ref().unwrap_or(ctes);
        let support = owned_support.as_ref().unwrap_or(inherited);
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
                    || (self.query(left, ctes, support)? == BackwardScanSupport::Native
                        && self.query(right, ctes, support)? == BackwardScanSupport::Native)
                {
                    Ok(BackwardScanSupport::Native)
                } else {
                    Ok(BackwardScanSupport::Unsupported)
                }
            }
            RelationalPlan::SetOp { .. } => Ok(BackwardScanSupport::Unsupported),
            RelationalPlan::QueryBlock(block) => self.block(block, ctes, support),
        }
    }

    fn block<S: Clone>(
        &self,
        block: &QueryBlockPlan,
        ctes: &CteScope<S>,
        support: &BindingMap<BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        if block.distinct || !block.distinct_on.is_empty() || !block.locking.is_empty() {
            return Ok(BackwardScanSupport::Unsupported);
        }
        let returns_set = projections_return_set(self.routines, block);
        if !block.order_by.is_empty()
            && has_effective_ordering(self.routines, block, self.params, ctes, self.control)?
        {
            if returns_set
                && !ordered_set_projection(self.routines, block, self.params, ctes, self.control)?
            {
                return Ok(BackwardScanSupport::Unsupported);
            }
            if matches!(block.compute, ComputePlan::Window)
                && window_preserves_order(self.routines, block, self.params, ctes, self.control)?
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
        support: &BindingMap<BackwardScanSupport>,
    ) -> Result<BackwardScanSupport, SQLError> {
        match source {
            SourcePlan::RetainedRows(_) => Ok(BackwardScanSupport::Materialize),
            SourcePlan::Table { name, .. } => {
                if let Some(traversal) = uqa_core::RelationIdentity::borrowed_reference(name)
                    .filter(|reference| reference.schema.is_none())
                    .and_then(|reference| support.get_name(reference.name))
                {
                    return Ok(*traversal);
                }
                if let Some(deferred) = ctes.deferred_reference(name) {
                    let query = deferred.body.query().ok_or_else(|| {
                        SQLError::Internal("deferred CTE has no query body".into())
                    })?;
                    let parent = ctes
                        .definition_scope_with_control(name, &self.control)?
                        .expect("declared CTE scope");
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
