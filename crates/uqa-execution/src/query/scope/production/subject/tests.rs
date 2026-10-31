//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog::test_support::empty_scope, query::CteScope};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{catalog::resolution::RelationLookupMode, plan::UnifiedPlan};

#[test]
fn transferred_authority_keeps_its_original_lease_through_scope_clones() {
    let budget = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let input = empty_scope();
    let mut scope = CteScope::<()>::with_catalog_with_control(
        input.catalog_read_view().unwrap(),
        input
            .relation_name_resolution()
            .unwrap()
            .copy_with_control(&control)
            .unwrap(),
        &control,
    )
    .unwrap();
    let parent = scope.privilege_subject().unwrap().clone();
    let authority = RoleReference::Named("v".repeat(4 << 20));
    let subject = authority.clone_with_control(&control).unwrap();
    let guard = scope
        .enter_privilege_subject_with_control(subject, &control)
        .unwrap();
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut child = None;
    let allocation = allocation_counter::measure(|| child = Some((*guard).clone()));
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let child = child.unwrap();
    drop(guard);
    assert_eq!(scope.privilege_subject().unwrap(), &parent);
    assert_eq!(child.privilege_subject().unwrap(), &authority);
    assert!(budget.used() >= occupied.bytes() + (4 << 20));
    drop(occupied);
    drop(scope);
    assert!(budget.used() >= 4 << 20);
    drop(child);
    assert_eq!(budget.used(), 0);
}

#[test]
fn query_authority_admission_restores_namespace_and_subject_on_every_exit() {
    let budget = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut scope = empty_scope();
    let subject = RoleReference::Named("parent".into());
    scope
        .set_privilege_subject_with_control(Some(&subject), &control)
        .unwrap();
    let baseline = budget.used();
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    query.relations_bound = true;
    query.relation_privilege_subject = Some(RoleReference::Named("x".repeat(4 << 20)));
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            scope
                .enter_query_plan_scope_with_control(&query, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
    assert_eq!(scope.privilege_subject().unwrap(), &subject);
    assert_eq!(
        scope.relation_lookup_mode(),
        Some(RelationLookupMode::Dynamic)
    );
    assert_eq!(budget.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            scope
                .enter_query_plan_scope_with_control(&query, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(scope.privilege_subject().unwrap(), &subject);
        assert_eq!(
            scope.relation_lookup_mode(),
            Some(RelationLookupMode::Dynamic)
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
    }
    let sibling = {
        let child = scope
            .enter_query_plan_scope_with_control(&query, &control)
            .unwrap();
        assert_eq!(
            child.privilege_subject().unwrap(),
            query.relation_privilege_subject.as_ref().unwrap()
        );
        assert_eq!(
            child.relation_lookup_mode(),
            Some(RelationLookupMode::Bound)
        );
        (*child).clone()
    };
    assert_eq!(scope.privilege_subject().unwrap(), &subject);
    assert_eq!(
        scope.relation_lookup_mode(),
        Some(RelationLookupMode::Dynamic)
    );
    assert!(budget.used() >= baseline + (4 << 20));
    drop(sibling);
    assert_eq!(budget.used(), baseline);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _child = scope
            .enter_query_plan_scope_with_control(&query, &control)
            .unwrap();
        panic!("source binding failed");
    }))
    .is_err());
    assert_eq!(scope.privilege_subject().unwrap(), &subject);
    assert_eq!(
        scope.relation_lookup_mode(),
        Some(RelationLookupMode::Dynamic)
    );
    assert_eq!(budget.used(), baseline);
    let mut empty = CteScope::<()>::new();
    let error = empty
        .enter_query_plan_scope_with_control(&query, &control)
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("has no statement name-resolution snapshot"));
    assert_eq!(budget.used(), baseline);
    drop(scope);
    assert_eq!(budget.used(), 0);
}

#[test]
fn transferred_authority_admission_is_atomic_and_releases_rejected_inputs() {
    let budget = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut scope = empty_scope();
    let parent = scope.privilege_subject().unwrap().clone();
    let authority = RoleReference::Named("v".repeat(4 << 20));
    let subject = authority.clone_with_control(&control).unwrap();
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        scope
            .enter_privilege_subject_with_control(subject, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(scope.privilege_subject().unwrap(), &parent);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        let subject = authority.clone_with_control(&control).unwrap();
        token.cancel();
        assert_eq!(
            scope
                .enter_privilege_subject_with_control(subject, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(scope.privilege_subject().unwrap(), &parent);
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let subject = authority.clone_with_control(&control).unwrap();
    let pointer = match &*subject {
        RoleReference::Named(name) => name.as_ptr(),
        RoleReference::Bound(_) => panic!("named"),
    };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard = scope
            .enter_privilege_subject_with_control(subject, &control)
            .unwrap();
        let RoleReference::Named(name) = guard.privilege_subject().unwrap() else {
            panic!("named")
        };
        assert_eq!(name.as_ptr(), pointer);
        panic!("view source construction failed");
    }))
    .is_err());
    assert_eq!(scope.privilege_subject().unwrap(), &parent);
    assert_eq!(budget.used(), 0);
}
