//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn cte() -> crate::plan::CtePlan {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile("WITH RECURSIVE path(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM path WHERE n<3) SEARCH DEPTH FIRST BY n SET ordered CYCLE n SET cycle USING visited SELECT * FROM path").unwrap().remove(0)) else { panic!("query") };
    query.ctes.remove(0)
}

fn query() -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT 'selected'::text AS n")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    *query
}

#[test]
fn selected_cte_query_transfer_preserves_root_identity_metadata_and_exact_leases() {
    let mut source = cte();
    let UnifiedPlan::Query(large) = UnifiedPlan::lower(
        crate::compile(&format!("SELECT '{}'::text AS n", "x".repeat(4 << 20)))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    source.body = crate::plan::CtePlanBody::Query(large);
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let selected =
        OwnedQueryPlan::from_produced(query().clone_with_control(&control).unwrap(), &control)
            .unwrap();
    let pointer = std::ptr::from_ref(selected.as_ref());
    let baseline = budget.used();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(source.copy_with_query(selected, &control).unwrap());
    });
    let output = output.unwrap();
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(std::ptr::from_ref(output.body.query().unwrap()), pointer);
    assert_eq!(output.name, source.name);
    assert_eq!(output.columns, source.columns);
    assert_eq!(output.recursive, source.recursive);
    assert_eq!(output.materialization, source.materialization);
    assert_eq!(
        serde_json::to_value(&output.search).unwrap(),
        serde_json::to_value(&source.search).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&output.cycle).unwrap(),
        serde_json::to_value(&source.cycle).unwrap()
    );
    assert_eq!(
        baseline as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_cte_query_transfer_releases_inputs_on_rejection_cancellation_and_unwind() {
    let mut source = cte();
    source.name = "x".repeat(4 << 20);
    let budget = MemoryBudget::new(16 << 20);
    let retained = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &retained, &invoking);
    let input = || {
        OwnedQueryPlan::from_produced(query().clone_with_control(&control).unwrap(), &control)
            .unwrap()
    };
    let selected = input();
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut error = None;
    let allocation = allocation_counter::measure(|| {
        error = Some(source.copy_with_query(selected, &control).unwrap_err());
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(error.unwrap().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&retained, &invoking] {
        let selected = input();
        token.cancel();
        assert_eq!(
            source
                .copy_with_query(selected, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _output = source.copy_with_query(input(), &control).unwrap();
        panic!("release selected CTE owners");
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
}
