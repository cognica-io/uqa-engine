//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable query inputs needed by tuple-lock rechecks.

use crate::query::CteScope;
use uqa_sql::plan::QueryBlockPlan;

/// Everything needed to rebuild the plan below this `LockRows` boundary for a tuple-local recheck. The statement is the query block as it existed before order-set rewrites; the rebuild replays the same construction the original pipeline used, so the recheck output matches the boundary schema.
pub struct LockRowsRecheckSource<S: Clone> {
    pub statement: QueryBlockPlan,
    pub ctes: CteScope<S>,
    pub ordered: bool,
    pub projections: Vec<crate::query::PhysicalProjection>,
}

impl<S: Clone> LockRowsRecheckSource<S> {
    pub fn new(statement: &QueryBlockPlan, ctes: &CteScope<S>, ordered: bool) -> Self {
        Self {
            statement: statement.clone(),
            ctes: ctes.clone(),
            ordered,
            projections: Vec::new(),
        }
    }

    pub fn with_projections(
        statement: &QueryBlockPlan,
        ctes: &CteScope<S>,
        ordered: bool,
        projections: Vec<crate::query::PhysicalProjection>,
    ) -> Self {
        Self {
            statement: statement.clone(),
            ctes: ctes.clone(),
            ordered,
            projections,
        }
    }
}
