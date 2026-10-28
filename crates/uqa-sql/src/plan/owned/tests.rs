//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::RelationalPlan, ScalarExpr};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn fixture() -> UnifiedPlan {
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT 'value'").unwrap().remove(0));
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    plan
}

#[test]
fn executable_query_projection_preserves_storage_and_last_reader_ownership() {
    let source = fixture();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.clone_executable_with_control(&control).unwrap();
    let required = copied.reserved_bytes();
    let UnifiedPlan::Query(query) = &*copied else {
        panic!("query")
    };
    let address = std::ptr::from_ref(query.as_ref());
    let mut owned = None;
    let allocation = allocation_counter::measure(|| {
        let query = OwnedExecutablePlan::from(copied).into_query().unwrap();
        assert_eq!(std::ptr::from_ref(query.as_ref()), address);
        assert_eq!(query.reserved_bytes(), required);
        owned = Some(query.into_executable());
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), required);
    let shared = owned.unwrap().into_shared(&control).unwrap();
    assert_eq!(budget.used(), required + size_of::<OwnedExecutablePlan>());
    let mut readers = std::array::from_fn::<_, 64, _>(|_| None);
    let allocation = allocation_counter::measure(|| {
        for reader in &mut readers {
            *reader = Some(shared.clone());
        }
    });
    assert_eq!(allocation.count_total, 0);
    assert!(readers
        .iter()
        .flatten()
        .all(|reader| reader.ptr_eq(&shared)));
    drop(shared);
    assert_eq!(budget.used(), required + size_of::<OwnedExecutablePlan>());
    drop(readers);
    assert_eq!(budget.used(), 0);
}

#[test]
fn query_root_and_shared_wrapper_reject_before_allocation_and_release_their_inputs() {
    let source = fixture();
    let token = CancellationToken::new();
    let baseline = MemoryBudget::new(8 * 1024 * 1024);
    let control = ProductionControl::new(&baseline, &token, &token);
    let copied = source.clone_executable_with_control(&control).unwrap();
    let required = copied.reserved_bytes();
    drop(copied);
    let exact = MemoryBudget::new(required);
    let control = ProductionControl::new(&exact, &token, &token);
    let copied = OwnedExecutablePlan::from(source.clone_executable_with_control(&control).unwrap());
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            copied.into_shared(&control).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(exact.used(), 0);
    let UnifiedPlan::Query(query) = source else {
        panic!("query")
    };
    let copied = query.clone_with_control(&control).unwrap();
    let occupied = exact.reserve(exact.available()).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            OwnedQueryPlan::from_produced(copied, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(exact.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(exact.used(), 0);
}

#[test]
fn cancelled_sharing_and_consumer_unwind_release_the_complete_executable() {
    let source = fixture();
    for cancelled in 0..3 {
        let budget = MemoryBudget::new(8 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let copied =
            OwnedExecutablePlan::from(source.clone_executable_with_control(&control).unwrap());
        match cancelled {
            0 => original.cancel(),
            1 => invoking.cancel(),
            _ => {}
        }
        if cancelled < 2 {
            assert_eq!(
                copied.into_shared(&control).unwrap_err().sqlstate(),
                Some("57014")
            );
        } else {
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _query = copied.into_query().unwrap();
                assert!(budget.used() >= 4 * 1024 * 1024);
                panic!("consumer unwind");
            }));
            assert!(failure.is_err());
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn unboxing_releases_only_the_original_root_allocation() {
    let source = fixture();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let query = OwnedExecutablePlan::from(source.clone_executable_with_control(&control).unwrap())
        .into_query()
        .unwrap();
    let required = budget.used();
    let mut unboxed = None;
    let allocation = allocation_counter::measure(|| {
        unboxed = Some(query.into_unboxed());
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_current, -(size_of::<QueryPlan>() as i64));
    assert_eq!(budget.used(), required - size_of::<QueryPlan>());
    let (query, memory) = unboxed.unwrap();
    assert_eq!(memory.reserved_bytes(), budget.used());
    drop(query);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn ordinary_lookup_shares_its_existing_syntax_without_copying() {
    let source = Arc::new(fixture());
    let address = Arc::as_ptr(&source);
    let mut readers = std::array::from_fn::<_, 64, _>(|_| None);
    let allocation = allocation_counter::measure(|| {
        for reader in &mut readers {
            *reader = Some(SharedExecutablePlan::from(source.clone()));
        }
    });
    assert_eq!(allocation.count_total, 0);
    for reader in readers.iter().flatten() {
        assert_eq!(std::ptr::from_ref(reader.as_ref()), address);
        assert_eq!(reader.reserved_bytes(), 0);
    }
    drop(source);
    assert!(readers
        .iter()
        .flatten()
        .all(|reader| reader.ptr_eq(readers[0].as_ref().unwrap())));
}
