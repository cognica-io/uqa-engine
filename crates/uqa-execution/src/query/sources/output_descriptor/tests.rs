//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn descriptor_traversal_borrows_identifiers_and_admits_only_completed_nodes() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let scope = crate::catalog::test_support::empty_scope();
    let uqa_sql::plan::UnifiedPlan::Query(mut query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("WITH q AS (SELECT 1) SELECT * FROM q")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let name = "x".repeat(4 << 20);
    query.ctes[0].name.clone_from(&name);
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Table {
        name: reference, ..
    }) = &mut block.from
    else {
        panic!("table")
    };
    reference.clone_from(&name);
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        assert!(!requires_physical_descriptor_with_control(&query, &scope, control).unwrap());
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        requires_physical_descriptor_with_control(&query, &scope, control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            requires_physical_descriptor_with_control(&query, &scope, control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}

#[test]
fn descriptor_cycles_keep_the_same_ancestor_chain_through_definition_scopes() {
    let mut scope = crate::catalog::test_support::empty_scope();
    let uqa_sql::plan::UnifiedPlan::Query(mut query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("WITH RECURSIVE q(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM q WHERE n < 3) SELECT * FROM q").unwrap().remove(0),
    ) else { panic!("query") };
    let declaration = query.ctes.remove(0);
    scope.insert_deferred(declaration.into());
    assert!(!requires_physical_descriptor(&query, &scope).unwrap());
}

#[test]
fn score_descriptor_follows_cte_definition_scope_and_demand() {
    let scope = crate::catalog::test_support::empty_scope();
    for (sql, required) in [
        (
            "SELECT id FROM docs WHERE text_match(body, 'rust')",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')), \
             retained AS (SELECT * FROM hits) \
             SELECT * FROM (WITH hits AS (VALUES (1)) SELECT DISTINCT * FROM retained) AS output",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) \
             SELECT * FROM (WITH hits AS (VALUES (1)) SELECT DISTINCT * FROM hits) AS output",
            false,
        ),
        (
            "WITH unused AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT 1",
            false,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT * FROM (WITH hits AS (SELECT * FROM hits) SELECT * FROM hits) s",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT * FROM (WITH retained AS (SELECT * FROM hits), hits AS (VALUES (1)) SELECT * FROM retained) s",
            true,
        ),
        (
            "WITH RECURSIVE numbers(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM numbers WHERE n < 3) SELECT * FROM numbers",
            false,
        ),
    ] {
        let uqa_sql::plan::UnifiedPlan::Query(query) =
            uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("expected query");
        };
        assert_eq!(requires_physical_descriptor(&query, &scope).unwrap(), required, "{sql}");
    }
}
