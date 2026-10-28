//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::plan::{AnalyzedSource, ExpressionPlan, QueryPlan, RelationalPlan, UnifiedPlan};
use crate::{RowSchema, ScalarExpr};
use std::sync::Arc;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn one(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[test]
fn executable_copies_preserve_query_mutation_and_cte_storage_shapes() {
    for sql in [
        "SELECT t.a AS named, (SELECT 1) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY named LIMIT 3 OFFSET 1 FOR UPDATE OF t",
        "SELECT sum(a) OVER w FROM t WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT a, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING count(*) > 0",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
        "VALUES (1, 'a'), (2, 'b')",
        "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
        "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH d AS (DELETE FROM t RETURNING a) INSERT INTO u SELECT a FROM d RETURNING a",
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
    ] {
        let source = one(sql);
        let expected = serde_json::to_value(&source).unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let copied = source.clone_executable_with_control(&control).unwrap();
        assert_eq!(serde_json::to_value(&*copied).unwrap(), expected, "{sql}");
        assert!(budget.used() > 0, "{sql}");
        drop(copied);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

fn payload_query(size: usize) -> QueryPlan {
    let UnifiedPlan::Query(mut query) = one("SELECT 'payload' AS value") else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(size)));
    block
        .privilege_columns
        .insert(crate::ColumnIdentity::qualified("table", "column"));
    query.analyzed_output = Some(RowSchema::with_types(
        vec!["value".into()],
        vec![Some(crate::ColumnType::Text)],
    ));
    query.analyzed_source = Some(Arc::new(AnalyzedSource {
        schema: RowSchema::default(),
        children: None,
    }));
    *query
}

#[test]
fn borrowed_query_copy_admits_one_executable_destination() {
    let query = payload_query(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let control = ProductionControl::new(&budget, &token, &token);
    let query_copy = query.clone_with_control(&control).unwrap();
    let required = query_copy.reserved_bytes() + size_of::<QueryPlan>();
    drop(query_copy);
    let reference = crate::plan::PlanRef::Query(&query);
    let exact = budget.child(required);
    let mut copied = None;
    let allocation = allocation_counter::measure(|| {
        copied = Some(
            reference
                .clone_executable_with_control(&ProductionControl::new(&exact, &token, &token))
                .unwrap(),
        );
    });
    let copied = copied.unwrap();
    assert_eq!(copied.reserved_bytes(), required);
    assert_eq!(allocation.bytes_current, required as i64);
    assert!(
        allocation.bytes_total < 4 * 1024 * 1024 + 16 * 1024,
        "{allocation:?}"
    );
    let UnifiedPlan::Query(destination) = &*copied else {
        panic!("query destination");
    };
    assert_eq!(
        serde_json::to_value(destination.as_ref()).unwrap(),
        serde_json::to_value(&query).unwrap()
    );
    assert!(std::sync::Arc::ptr_eq(
        destination.analyzed_source.as_ref().unwrap(),
        query.analyzed_source.as_ref().unwrap()
    ));
    drop(copied);
    assert_eq!(budget.used(), 0);
    let insufficient = budget.child(required - 1);
    assert_eq!(
        reference
            .clone_executable_with_control(&ProductionControl::new(&insufficient, &token, &token))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn executable_payload_copies_admit_actual_destinations_and_release_every_failure() {
    for size in [1, 4 * 1024 * 1024] {
        let source = payload_query(size);
        let token = CancellationToken::new();
        let budget = MemoryBudget::new(8 * 1024 * 1024);
        let control = ProductionControl::new(&budget, &token, &token);
        let copied = source.clone_with_control(&control).unwrap();
        let required = copied.reserved_bytes();
        assert!(required >= size);
        assert_eq!(budget.used(), required);
        assert!(Arc::ptr_eq(
            source.analyzed_source.as_ref().unwrap(),
            copied.analyzed_source.as_ref().unwrap()
        ));
        assert_eq!(
            serde_json::to_value(&*copied).unwrap(),
            serde_json::to_value(&source).unwrap()
        );
        drop(copied);
        assert_eq!(budget.used(), 0);
        for allowance in [0, 1, required / 2, required - 1] {
            let small = MemoryBudget::new(allowance);
            assert!(source
                .clone_with_control(&ProductionControl::new(&small, &token, &token))
                .is_err());
            assert_eq!(small.used(), 0, "{size}, {allowance}");
        }
        let exact = MemoryBudget::new(required);
        drop(
            source
                .clone_with_control(&ProductionControl::new(&exact, &token, &token))
                .unwrap(),
        );
        assert_eq!(exact.used(), 0);
        for cancel_original in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let measured = allocation_counter::measure(|| {
                assert!(source
                    .clone_with_control(&ProductionControl::new(&budget, &original, &invoking))
                    .is_err());
            });
            assert!(measured.bytes_total < 1024, "{measured:?}");
            assert_eq!(budget.used(), 0);
        }
        let small = MemoryBudget::new(1024);
        let measured = allocation_counter::measure(|| {
            let result = source.clone_with_control(&ProductionControl::new(&small, &token, &token));
            if size > 1024 {
                assert!(result.is_err());
            }
        });
        assert!(measured.bytes_total < 64 * 1024, "{measured:?}");
        assert_eq!(small.used(), 0);
    }
}

#[test]
fn scalar_arena_copy_retains_query_slots_and_utility_commands_use_their_own_owner() {
    let expression = ExpressionPlan {
        scalar: ScalarExpr::ScalarSubquery(0),
        subqueries: vec![payload_query(1024)],
    };
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(64 * 1024);
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = expression.clone_with_control(&control).unwrap();
    assert_eq!(copied.scalar, ScalarExpr::ScalarSubquery(0));
    assert_eq!(copied.subqueries.len(), 1);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let utility = one("CREATE TABLE t(a integer)");
    assert!(utility.clone_executable_with_control(&control).is_err());
    assert_eq!(budget.used(), 0);
    assert!(utility
        .clone_executable_with_control(&ProductionControl::uncontrolled())
        .is_err());
}
