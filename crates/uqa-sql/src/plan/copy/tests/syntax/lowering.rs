//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Statement, plan::NoRegisteredAggregates};

fn verify_lowered(sql: &str) {
    let source = crate::compile(sql).unwrap().remove(0);
    let expected = UnifiedPlan::lower(source.clone());
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let lowered =
            UnifiedPlan::lower_borrowed_for_rewrite(&source, &NoRegisteredAggregates, &control)
                .unwrap();
        let lowered = lowered
            .map_queries_while(&control, &mut |query| Ok((query, true)))
            .unwrap();
        output = Some(lowered.finish(&control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(
        serde_json::to_value(&*output).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "{sql}"
    );
    assert_eq!(format!("{:?}", *output), format!("{expected:?}"), "{sql}");
    assert_eq!(output.reserved_bytes(), budget.used(), "{sql}");
    assert_eq!(
        allocation.bytes_current,
        budget.used() as i64,
        "{sql}: {allocation:?}"
    );
    drop(output);
    assert_eq!(budget.used(), 0, "{sql}");
}

#[test]
fn admitted_lowering_preserves_statements_and_scalar_query_namespaces() {
    for sql in super::fixtures::STATEMENTS.iter().copied().chain([
        "SELECT (SELECT 1), sum((SELECT 2)) OVER w FROM LATERAL (SELECT 3) AS t(a) WINDOW w AS (ORDER BY (SELECT 4))",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT (SELECT 2) OFFSET (SELECT 3)",
        "WITH q AS (SELECT (SELECT 1)) SELECT * FROM q",
        "WITH d AS (DELETE FROM t RETURNING (SELECT 1)) INSERT INTO u SELECT * FROM d RETURNING (SELECT 2)",
        "INSERT INTO t(a[1]) VALUES ((SELECT 1)) ON CONFLICT ((a + (SELECT 2))) WHERE a > (SELECT 3) DO UPDATE SET a = (SELECT 4) WHERE a < (SELECT 5) RETURNING (SELECT 6)",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[(SELECT 3)] = (SELECT 4) FROM (SELECT 5) u WHERE a = (SELECT 6) RETURNING (SELECT 7)",
        "MERGE INTO t USING (SELECT 1 AS a) u ON t.a = (SELECT 2) WHEN MATCHED AND a > (SELECT 3) THEN UPDATE SET a = (SELECT 4) WHEN NOT MATCHED THEN INSERT(a) VALUES((SELECT 5)) RETURNING (SELECT 6)",
        "SELECT * FROM ROWS FROM (generate_series((SELECT 1), 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
    ]) {
        verify_lowered(sql);
    }
}

#[test]
fn first_executable_destination_rejects_large_inputs_before_copying_them() {
    let Statement::Select(mut query) = crate::compile("SELECT 'small', (SELECT 'large')")
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    let crate::ast::Expr::ScalarSubquery(inner) = &mut query.projections[1].expr else {
        panic!("subquery")
    };
    inner.projections[0].expr = crate::ast::Expr::Literal(Value::Str("x".repeat(4 << 20)));
    let source = Statement::Select(query);
    let budget = MemoryBudget::new(1 << 20);
    let sibling = budget.reserve(512).unwrap();
    let allowance = budget.child(16 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&allowance, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        let error =
            UnifiedPlan::lower_borrowed_for_rewrite(&source, &NoRegisteredAggregates, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_max < 32 << 10, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(allowance.used(), 0);
    assert_eq!(budget.used(), 512);
    drop(sibling);
}

#[test]
fn first_executable_destination_observes_both_cancellations_and_recovers() {
    let source = crate::compile("SELECT (SELECT 1)").unwrap().remove(0);
    let budget = MemoryBudget::new(1 << 20);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            let error =
                UnifiedPlan::lower_borrowed_for_rewrite(&source, &NoRegisteredAggregates, &control)
                    .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output =
        UnifiedPlan::lower_borrowed_for_rewrite(&source, &NoRegisteredAggregates, &control)
            .unwrap();
    assert!(output.reserved_bytes() > 0);
    drop(output);
    assert_eq!(budget.used(), 0);
}
