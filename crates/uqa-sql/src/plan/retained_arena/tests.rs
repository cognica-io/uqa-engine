//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{RelationalPlan, UnifiedPlan};
use crate::ScalarExpr;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}
fn payload(bytes: usize) -> QueryPlan {
    let mut query = query("SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(bytes)));
    query
}

#[test]
fn retained_query_arenas_keep_exact_constructor_ownership_until_the_last_reader() {
    let input = [
        query("WITH q AS (SELECT 7 AS id) SELECT id, (SELECT 11) FROM q"),
        query("VALUES(1),(2) UNION ALL SELECT 3"),
        payload(4 << 20),
    ];
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("independent").unwrap();
    let baseline = budget.used();
    let owned = ScalarQueryArena::copy_from(&input, &control).unwrap();
    let address = owned.queries().as_ptr();
    let retained = owned.into_shared(&control).unwrap();
    assert_eq!(retained.as_ptr(), address);
    assert_eq!(budget.used(), baseline + retained.reserved_bytes());
    assert!(retained.reserved_bytes() > 4 << 20);
    assert_eq!(
        serde_json::to_value(&retained[..2]).unwrap(),
        serde_json::to_value(&input[..2]).unwrap()
    );
    let allocated = budget.used();
    let mut readers = Vec::with_capacity(32);
    let observed = allocation_counter::measure(|| {
        for _ in 0..32 {
            readers.push(retained.clone());
        }
    });
    assert_eq!(observed.count_total, 0);
    assert_eq!(budget.used(), allocated);
    assert!(readers.iter().all(|reader| retained.ptr_eq(reader)));
    drop(retained);
    while readers.len() > 1 {
        readers.pop();
        assert_eq!(budget.used(), allocated);
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _last = readers.pop().unwrap();
        panic!("arena reader unwind");
    }))
    .is_err());
    assert_eq!(budget.used(), baseline);
    assert_eq!(&**sibling, "independent");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn query_arena_admission_rejects_large_payloads_and_retires_partial_shared_destinations() {
    let input = [payload(4 << 20)];
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    for available in [0, 64, 1024, 16 << 10] {
        let occupied = budget.reserve(budget.available() - available).unwrap();
        let observed = allocation_counter::measure(|| {
            assert_eq!(
                RetainedQueryArena::capture(&input, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(observed.bytes_total < 64 << 10, "{observed:?}");
        assert_eq!(budget.used(), 37 + occupied.bytes());
        drop(occupied);
    }
    let destination = ScalarQueryArena::copy_from(&input, &control)
        .unwrap()
        .finish(&control)
        .unwrap();
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        RetainedQueryArena::from_produced(destination, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 37 + occupied.bytes());
    drop(occupied);
    drop(RetainedQueryArena::capture(&input, &control).unwrap());
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn query_arena_capture_preserves_both_cancellations_and_allocation_free_empty_namespaces() {
    let input = [payload(4096)];
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(37).unwrap();
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            RetainedQueryArena::capture(&input, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            RetainedQueryArena::capture(&[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 37);
        token.reset();
        let copied = ScalarQueryArena::copy_from(&input, &control)
            .unwrap()
            .finish(&control)
            .unwrap();
        token.cancel();
        assert_eq!(
            RetainedQueryArena::from_produced(copied, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 37);
        token.reset();
    }
    let occupied = budget.reserve(budget.available()).unwrap();
    let observed = allocation_counter::measure(|| {
        let empty = RetainedQueryArena::capture(&[], &control).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.reserved_bytes(), 0);
        assert!(empty.ptr_eq(&empty.clone()));
    });
    assert_eq!(observed.count_total, 0);
    drop(occupied);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_binding_snapshots_admit_ordinary_arenas_and_share_previously_owned_queries() {
    let input = [payload(4 << 20)];
    let mut snapshot = crate::binding::snapshot::BindingSnapshot::from(
        crate::binding::fixture::empty_binding_context(),
    );
    snapshot.scalar_subqueries = input.as_slice().into();
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let occupied = budget.reserve(budget.available() - (16 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        assert!(
            matches!(snapshot.copy_with_control(&control), Err(error) if error.sqlstate() == Some("53200"))
        );
    });
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    let captured = snapshot.copy_with_control(&control).unwrap();
    assert!(!captured
        .scalar_subqueries
        .ptr_eq(&snapshot.scalar_subqueries));
    assert!(budget.used() > 4 << 20);
    snapshot.scalar_subqueries = captured.scalar_subqueries.clone();
    let mut other = None;
    let observed = allocation_counter::measure(|| {
        other = Some(snapshot.copy_with_control(&control).unwrap());
    });
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    let other = other.unwrap();
    assert!(other.scalar_subqueries.ptr_eq(&captured.scalar_subqueries));
    drop(snapshot);
    drop(captured);
    assert!(budget.used() > 4 << 20);
    drop(other);
    assert_eq!(budget.used(), 0);
}
