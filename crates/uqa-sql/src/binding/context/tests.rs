//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::binding::{fixture::empty_binding_context, snapshot::BindingSnapshot, SchemaScope};
use crate::plan::{QueryPlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query");
    };
    *plan
}

#[test]
fn binding_scope_copies_do_not_duplicate_immutable_plan_payloads() {
    let text = "x".repeat(128 * 1024);
    let mut definition = query(&format!(
        "WITH source AS (SELECT '{text}'::text AS value) SELECT value FROM source"
    ));
    let subqueries = [query(&format!("SELECT '{text}'::text"))];
    let mut context = empty_binding_context();
    let cte = definition.ctes.remove(0);
    context.deferred_ctes.insert(cte.name.clone(), cte.into());
    context.scalar_subqueries = &subqueries;
    let snapshot = BindingSnapshot::from(context);

    let allocation = allocation_counter::measure(|| {
        let copies = (0..16)
            .map(|_| {
                let copy = snapshot.clone();
                let context = copy.context();
                let schema = SchemaScope::from_context(&context).unwrap();
                assert!(schema.deferred_ctes.contains_key("source"));
                assert_eq!(context.scalar_subqueries.len(), 1);
                copy
            })
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 16);
    });
    assert!(
        allocation.bytes_total < 256 * 1024 && allocation.bytes_max < 128 * 1024,
        "binding scope copies duplicate immutable plans: {allocation:?}"
    );

    let mut child = snapshot.clone();
    child.deferred_ctes.remove("source");
    assert!(snapshot.deferred_ctes.contains_key("source"));
    assert!(!child.deferred_ctes.contains_key("source"));
}
