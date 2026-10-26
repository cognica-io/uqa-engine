//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{order_statement_ctes_with_control, schedule_plan_ctes_with_control};
use crate::{
    ast::{FunctionBinding, FunctionVolatility},
    plan::{QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    semantics::volatility::VolatilityCatalog,
    SQLError,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

struct Catalog<'a>(Option<&'a CancellationToken>);
impl VolatilityCatalog for Catalog<'_> {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        if name == "cancel_me" {
            if let Some(token) = self.0 {
                token.cancel();
            }
            return Some(FunctionVolatility::Immutable);
        }
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        if name == "catalog_failure" {
            return Err(SQLError::Internal("metadata unavailable".into()));
        }
        Ok(None)
    }
}

fn query(sql: &str) -> Box<QueryPlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    query
}

#[test]
fn admitted_schedules_preserve_syntactic_consumers_lexical_shadowing_and_effects() {
    for (sql, expected) in [
        ("WITH q AS (VALUES(1)) SELECT * FROM q", vec![("q", true)]),
        ("WITH q AS (SELECT random()) SELECT 7", vec![]),
        ("WITH q AS (SELECT random()) SELECT * FROM q", vec![("q", false)]),
        ("WITH q AS MATERIALIZED (VALUES(1)) SELECT * FROM q", vec![("q", false)]),
        ("WITH q AS NOT MATERIALIZED (VALUES(1)) SELECT * FROM q a, q b", vec![("q", true)]),
        ("WITH q AS (VALUES(1)), unused AS (SELECT * FROM q) SELECT * FROM q", vec![("q", false)]),
        ("WITH q AS (VALUES(1)) SELECT * FROM (WITH q AS (SELECT * FROM q) SELECT * FROM q) s", vec![("q", true)]),
        ("WITH q AS (VALUES(1)) SELECT * FROM (WITH RECURSIVE q AS (VALUES(2)) SELECT * FROM q) s", vec![]),
        ("WITH q AS (VALUES(1)) SELECT * FROM public.q", vec![]),
        ("WITH RECURSIVE a AS (SELECT * FROM c), b AS (VALUES(2)), c AS (VALUES(1)) SELECT * FROM a,b", vec![("b", true), ("c", true), ("a", true)]),
        ("WITH RECURSIVE q(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM q WHERE n<3) SELECT * FROM q", vec![("q", false)]),
        ("WITH a AS (VALUES(1)), written AS (INSERT INTO t SELECT * FROM a), unused AS (SELECT random()) SELECT 7", vec![("a", true), ("written", false)]),
        ("WITH q AS (SELECT * FROM catalog_failure) SELECT * FROM q", vec![("q", false)]),
    ] {
        let plan = query(sql);
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let result = schedule_plan_ctes_with_control(&Catalog(None), &plan, ProductionControl::new(&memory, &token, &token)).unwrap();
        assert_eq!(result.iter().map(|cte| (cte.plan.name.as_str(), cte.deferred)).collect::<Vec<_>>(), expected, "{sql}");
        for cte in result.iter() { assert!(plan.ctes.iter().any(|original| std::ptr::eq(original, cte.plan))); }
        if !result.is_empty() { assert!(memory.used() > 0); }
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn schedule_memory_is_independent_of_borrowed_identifier_length() {
    let mut plan = query("WITH q AS (VALUES(1)) SELECT * FROM q");
    plan.ctes[0].name = "x".repeat(64 * 1024);
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("query block");
    };
    let Some(SourcePlan::Table { name, .. }) = &mut block.from else {
        panic!("table source");
    };
    name.clone_from(&plan.ctes[0].name);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        let scheduled = schedule_plan_ctes_with_control(
            &Catalog(None),
            &plan,
            ProductionControl::new(&memory, &token, &token),
        )
        .unwrap();
        assert_eq!(scheduled.len(), 1);
        assert!(scheduled[0].deferred);
        assert!(std::ptr::eq(scheduled[0].plan, &raw const plan.ctes[0]));
        assert!(memory.used() > 0);
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert!(memory.peak() < 4096);
    assert_eq!(memory.used(), 0);
}

#[test]
fn rejected_schedules_release_scratch_and_results_under_both_cancellation_owners() {
    let plan = query("WITH a AS (VALUES(1)), b AS (SELECT * FROM a), c AS MATERIALIZED (SELECT * FROM b) SELECT * FROM c");
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let parent = MemoryBudget::new(65536);
    let mut failed = 0;
    let mut succeeded = 0;
    for bytes in [0, 16, 64, 128, 256, 512, 1024, 2048, 4096, 8192] {
        let memory = parent.child(bytes);
        let result = schedule_plan_ctes_with_control(
            &Catalog(None),
            &plan,
            ProductionControl::new(&memory, &original, &invoking),
        );
        match result {
            Ok(result) => {
                succeeded += 1;
                assert_eq!(result.len(), 3);
                drop(result);
            }
            Err(error) => {
                failed += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(parent.used(), 0);
    }
    assert!(failed > 0 && succeeded > 0);
    for token in [&original, &invoking] {
        token.cancel();
        let error = schedule_plan_ctes_with_control(
            &Catalog(None),
            &plan,
            ProductionControl::new(&parent, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(parent.used(), 0);
        token.reset();
        let during = query("WITH q AS (SELECT cancel_me()) SELECT * FROM q");
        let error = schedule_plan_ctes_with_control(
            &Catalog(Some(token)),
            &during,
            ProductionControl::new(&parent, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(parent.used(), 0);
        token.reset();
    }
}

#[test]
fn postponed_statement_results_keep_their_allowance_until_both_arrays_are_released() {
    let plan = query("WITH a AS (VALUES(1)), i1 AS (INSERT INTO t SELECT * FROM a RETURNING n), i2 AS (INSERT INTO t VALUES(2) RETURNING n), q AS (VALUES(3)) SELECT * FROM q");
    let token = CancellationToken::new();
    let mut successes = 0;
    let mut failures = 0;
    for bytes in [0, 64, 128, 256, 512, 1024, 2048, 4096, 8192] {
        let memory = MemoryBudget::new(bytes);
        let result = order_statement_ctes_with_control(
            &plan.ctes,
            plan.ctes.iter(),
            ["q"],
            ProductionControl::new(&memory, &token, &token),
        );
        match result {
            Ok(result) => {
                successes += 1;
                assert_eq!(
                    result
                        .primary
                        .iter()
                        .map(|cte| cte.name.as_str())
                        .collect::<Vec<_>>(),
                    ["q"]
                );
                assert_eq!(
                    result
                        .postponed
                        .iter()
                        .map(|cte| cte.name.as_str())
                        .collect::<Vec<_>>(),
                    ["i2", "a", "i1"]
                );
                assert!(memory.used() >= 4 * size_of::<&crate::plan::CtePlan>());
                drop(result);
            }
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(successes > 0 && failures > 0);
}
