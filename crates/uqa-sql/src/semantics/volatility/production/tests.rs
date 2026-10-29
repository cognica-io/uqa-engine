//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{QueryPlan, UnifiedPlan};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Views {
    first: QueryPlan,
    second: QueryPlan,
    reads: Cell<usize>,
    fail: Cell<bool>,
}
fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    *query
}
impl Views {
    fn new() -> Self {
        Self {
            first: query("SELECT * FROM second"),
            second: query("SELECT * FROM first UNION ALL SELECT nextval('ticks')"),
            reads: Cell::new(0),
            fail: Cell::new(false),
        }
    }
}
impl VolatilityCatalog for Views {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        panic!("native view must remain borrowed")
    }
    fn with_view_query(
        &self,
        name: &str,
        read: &mut dyn FnMut(&QueryPlan) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        if self.fail.get() {
            return Err(SQLError::Internal(
                "injected view observation failure".into(),
            ));
        }
        match name {
            "first" => read(&self.first).map(Some),
            "second" => read(&self.second).map(Some),
            _ => Ok(None),
        }
    }
}

#[test]
fn view_cycles_and_cte_shadowing_share_borrowed_queries_and_retire_scratch() {
    let catalog = Views::new();
    let root = query("SELECT * FROM first");
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(query_contains_volatile_function_with_control(&catalog, &root, &control).unwrap());
    assert_eq!(catalog.reads.get(), 2);
    assert_eq!(budget.used(), 0);
    let shadowed = query("WITH first AS (VALUES(1)) SELECT * FROM first");
    assert!(!query_contains_volatile_function_with_control(&catalog, &shadowed, &control).unwrap());
    assert_eq!(catalog.reads.get(), 2);
    assert_eq!(budget.used(), 0);
    catalog.fail.set(true);
    assert!(matches!(
        query_contains_volatile_function_with_control(&catalog, &root, &control),
        Err(SQLError::Internal(message)) if message == "injected view observation failure"
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_entry_admission_and_both_cancellations_precede_catalog_callbacks() {
    let catalog = Views::new();
    let root = query("SELECT * FROM first");
    let budget = MemoryBudget::new(8);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        query_contains_volatile_function_with_control(&catalog, &root, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(catalog.reads.get(), 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                query_contains_volatile_function_with_control(&catalog, &root, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(catalog.reads.get(), 0);
        assert_eq!(budget.used(), 0);
    }
}
