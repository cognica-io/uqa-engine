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
