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
    pub statement: uqa_core::memory::Produced<QueryBlockPlan>,
    pub ctes: CteScope<S>,
    pub ordered: bool,
    pub projections: Vec<crate::query::PhysicalProjection>,
}

/// Borrow construction inputs until the owning lock boundary establishes that tuple rechecks are needed. Rewritten ordering still supplies the original query block.
pub struct LockRowsRecheckInput<'a, S: Clone> {
    statement: &'a QueryBlockPlan,
    ctes: &'a CteScope<S>,
    ordered: bool,
    projections: Vec<crate::query::PhysicalProjection>,
    resources: uqa_core::memory::OwnedProductionControl,
}

impl<'a, S: Clone> LockRowsRecheckInput<'a, S> {
    pub fn new(
        statement: &'a QueryBlockPlan,
        ctes: &'a CteScope<S>,
        ordered: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Self {
        Self::with_projections(statement, ctes, ordered, Vec::new(), control)
    }

    pub fn with_projections(
        statement: &'a QueryBlockPlan,
        ctes: &'a CteScope<S>,
        ordered: bool,
        projections: Vec<crate::query::PhysicalProjection>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Self {
        Self {
            statement,
            ctes,
            ordered,
            projections,
            resources: control.capture(),
        }
    }

    pub(super) fn retain(self) -> Result<LockRowsRecheckSource<S>, uqa_sql::SQLError> {
        let statement = self
            .statement
            .clone_with_control(&self.resources.control())?;
        Ok(LockRowsRecheckSource {
            statement,
            ctes: self.ctes.clone(),
            ordered: self.ordered,
            projections: self.projections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_lock_recheck_inputs_borrow_the_original_statement_and_scope() {
        let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
            uqa_sql::compile(&format!(
                "SELECT '{}'::text AS payload",
                "x".repeat(4 << 20)
            ))
            .unwrap()
            .remove(0),
        ) else {
            panic!("query")
        };
        let uqa_sql::plan::RelationalPlan::QueryBlock(statement) = &query.root else {
            panic!("block")
        };
        let scope = CteScope::<()>::new();
        let allocation = allocation_counter::measure(|| {
            let input = LockRowsRecheckInput::new(
                statement,
                &scope,
                false,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            );
            assert!(std::ptr::eq(input.statement, statement.as_ref()));
            assert!(std::ptr::eq(input.ctes, std::ptr::from_ref(&scope)));
        });
        assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    }
}
