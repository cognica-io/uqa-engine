//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn deferred_cte_scopes_borrow_plan_payloads_and_share_their_lexical_ancestors() {
    let sql = format!(
        "WITH source AS (SELECT '{}' AS value) SELECT * FROM source",
        "x".repeat(1024 * 1024)
    );
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0))
    else {
        panic!("query required")
    };
    let definition = &plan.ctes[0];
    let names = (0..128)
        .map(|index| format!("cte_{index:03}"))
        .collect::<Vec<_>>();
    let memory = MemoryBudget::new(512 * 1024);
    let mut scope = Scope::new(&memory);
    let allocations = allocation_counter::measure(|| {
        for name in &names {
            scope
                .try_insert(
                    name.as_str(),
                    Some(DeferredCte {
                        plan: definition,
                        outer: scope.clone(),
                    }),
                )
                .unwrap();
        }
    });
    assert!(allocations.bytes_max < 512 * 1024, "{allocations:?}");
    assert_eq!(scope.len(), names.len());
    let last = scope
        .get(names.last().unwrap().as_str())
        .unwrap()
        .as_ref()
        .unwrap();
    assert!(std::ptr::eq(last.plan, definition));
    assert_eq!(last.outer.len(), names.len() - 1);
    assert!(last.outer.get(names.last().unwrap().as_str()).is_none());
    let root = scope.clone();
    let before = memory.used();
    let snapshot = scope.clone();
    assert_eq!(memory.used(), before);
    drop((snapshot, root, scope));
    assert_eq!(memory.used(), 0);
}
