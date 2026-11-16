//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::{Expr, FromClause, Statement},
    plan::{NoRegisteredAggregates, UnifiedPlan},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn deep() -> Expr {
    let mut value = Expr::Literal(Value::Bool(true));
    for _ in 0..8192 {
        value = Expr::Not(Box::new(value));
    }
    value
}

fn statement(sql: &str) -> Statement {
    crate::compile(sql).unwrap().remove(0)
}

#[test]
fn enclosing_query_and_mutation_failures_retire_completed_deep_scalar_prefixes() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for sql in [
                "SELECT true WHERE true",
                "SELECT * FROM (VALUES (true)) AS a(v) JOIN (VALUES (1)) AS b(v) ON true",
                "INSERT INTO t VALUES (true) RETURNING 1",
                "UPDATE t SET a = true RETURNING 1",
                "DELETE FROM t WHERE true RETURNING 1",
                "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            ] {
                let mut source = statement(sql);
                let rejected = "x".repeat(32 << 20);
                match &mut source {
                    Statement::Select(query) if query.from.is_none() => {
                        query.projections[0].expr = deep();
                        query.r#where = Some(Expr::Literal(Value::Str(rejected)));
                    }
                    Statement::Select(query) => {
                        let Some(FromClause::Join { left, right, .. }) = &mut query.from else {
                            panic!("join fixture")
                        };
                        let FromClause::Values { rows, .. } = left.as_mut() else {
                            panic!("left values")
                        };
                        rows[0][0] = deep();
                        let FromClause::Values { alias, .. } = right.as_mut() else {
                            panic!("right values")
                        };
                        *alias = Some(rejected);
                    }
                    Statement::Insert(value) => {
                        value.rows[0][0] = deep();
                        value.target_qualifier = rejected;
                    }
                    Statement::Update(value) => {
                        value.assignments[0].1 = deep();
                        value.target_qualifier = rejected;
                    }
                    Statement::Delete(value) => {
                        value.r#where = Some(deep());
                        value.target_qualifier = rejected;
                    }
                    Statement::Merge(value) => {
                        value.join_condition = deep();
                        value.target_qualifier = rejected;
                    }
                    _ => unreachable!(),
                }
                let memory = MemoryBudget::new(16 << 20);
                let sibling = memory.reserve(37).unwrap();
                let token = CancellationToken::new();
                let control = ProductionControl::new(&memory, &token, &token);
                let observed = AtomicUsize::new(0);
                let parameters = |_: &Expr| {
                    observed.fetch_add(1, Ordering::Relaxed);
                    None
                };
                let allocation = allocation_counter::measure(|| {
                    let error = UnifiedPlan::lower_borrowed_with_parameters(
                        &source,
                        &NoRegisteredAggregates,
                        &control,
                        Some(&parameters),
                    )
                    .unwrap_err();
                    assert_eq!(error.sqlstate(), Some("53200"), "{sql}: {error}");
                });
                assert!(observed.load(Ordering::Relaxed) > 8192, "{sql}");
                assert_eq!(allocation.bytes_current, 0, "{sql}: {allocation:?}");
                assert_eq!(memory.used(), 37, "{sql}");
                drop(sibling);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn enclosing_constructors_retire_each_cancelled_prefix_and_preserve_siblings() {
    for sql in [
        "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT (SELECT 3) FROM (VALUES (4)) v(x) JOIN (VALUES (5)) w(y) ON x = y WHERE x > 6",
        "SELECT 1 UNION ALL SELECT 2 ORDER BY (SELECT 3) LIMIT (SELECT 4) OFFSET (SELECT 5)",
        "WITH a AS (SELECT 1) INSERT INTO t VALUES (2), ((SELECT 3)) RETURNING (SELECT 4)",
        "WITH a AS (SELECT 1) UPDATE t SET a = (SELECT 2) FROM (VALUES (3)) v(x) WHERE a = 4 RETURNING (SELECT 5)",
        "WITH a AS (SELECT 1) DELETE FROM t USING (VALUES (2)) v(x) WHERE a = (SELECT 3) RETURNING (SELECT 4)",
        "WITH a AS (SELECT 1) MERGE INTO t USING (VALUES (2)) v(x) ON a = (SELECT 3) WHEN MATCHED THEN UPDATE SET a = (SELECT 4) RETURNING (SELECT 5)",
        "PREPARE p AS SELECT (SELECT 1), (SELECT 2)",
        "EXPLAIN SELECT (SELECT 1), (SELECT 2)",
        "CALL p((SELECT 1), (SELECT 2), (SELECT 3))",
    ] {
        let source = statement(sql);
        let memory = MemoryBudget::new(8 << 20);
        let sibling = memory.reserve(37).unwrap();
        let count = AtomicUsize::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let parameters = |_: &Expr| { count.fetch_add(1, Ordering::Relaxed); None };
        drop(UnifiedPlan::lower_borrowed_with_parameters(&source, &NoRegisteredAggregates, &control, Some(&parameters)).unwrap());
        for prefix in 0..count.load(Ordering::Relaxed) {
            for original in [false, true] {
                let allocation = allocation_counter::measure(|| {
                    let original_token = CancellationToken::new();
                    let invoking_token = CancellationToken::new();
                    let control = ProductionControl::new(&memory, &original_token, &invoking_token);
                    let visited = AtomicUsize::new(0);
                    let parameters = |_: &Expr| {
                        if visited.fetch_add(1, Ordering::Relaxed) == prefix {
                            if original { original_token.cancel(); } else { invoking_token.cancel(); }
                        }
                        None
                    };
                    let error = UnifiedPlan::lower_borrowed_with_parameters(&source, &NoRegisteredAggregates, &control, Some(&parameters)).unwrap_err();
                    assert_eq!(error.sqlstate(), Some("57014"), "{sql}: {prefix}: {error}");
                });
                assert_eq!(allocation.bytes_current, 0, "{sql}: {prefix}");
                assert_eq!(memory.used(), 37, "{sql}: {prefix}");
            }
        }
        drop(sibling);
    }
}
