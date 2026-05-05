//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planner-to-physical bridge. Wraps the chosen [`JoinPlan`] alongside
//! the optimised [`SelectStmt`] so the engine can hand the bundle to
//! the execution layer.
//!
//! The bridge stays small and free of policy on purpose -- the
//! engine still owns the row sources, projections, and final
//! assembly. This struct just packages the planner's outputs into a
//! single value the engine can route through `Engine::execute_plan`
//! once the engine is fully Volcano-driven.

use uqa_sql::ast::SelectStmt;

use crate::join_enumerator::JoinPlan;

#[derive(Debug, Clone)]
pub struct PlannedQuery {
    pub stmt: SelectStmt,
    pub join_plan: Option<JoinPlan>,
}

impl PlannedQuery {
    pub fn new(stmt: SelectStmt) -> Self {
        Self {
            stmt,
            join_plan: None,
        }
    }

    pub fn with_join_plan(mut self, plan: JoinPlan) -> Self {
        self.join_plan = Some(plan);
        self
    }
}
