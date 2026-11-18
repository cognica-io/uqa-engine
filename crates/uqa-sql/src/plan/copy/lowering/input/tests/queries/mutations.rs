//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_mutation_roots_preserve_complete_fields_and_query_scopes() {
    assert_equivalent([
        "WITH c AS (SELECT (SELECT 1)) INSERT INTO t(a[(SELECT 2)]) VALUES ((SELECT 3)), (NOT ((SELECT 4) = 5)) ON CONFLICT (a) WHERE a > (SELECT 6) DO UPDATE SET a[(SELECT 7)] = (SELECT 8) WHERE a > (SELECT 9) RETURNING WITH (OLD AS before, NEW AS after) (SELECT 10), before.a, after.a",
        "WITH c AS (SELECT 1) INSERT INTO t(a) SELECT (SELECT 2) FROM (SELECT (SELECT 3)) s RETURNING (SELECT 4)",
        "INSERT INTO t DEFAULT VALUES ON CONFLICT DO NOTHING",
        "WITH c AS (SELECT (SELECT 1)) UPDATE ONLY t AS target SET a[(SELECT 2):(SELECT 3)].x = (SELECT 4), (b, c) = (SELECT 5, 6) FROM (SELECT (SELECT 7)) s WHERE a = (SELECT 8) RETURNING WITH (NEW AS after) (SELECT 9), after.a",
        "WITH c AS (SELECT (SELECT 1)) DELETE FROM ONLY t AS target USING (SELECT (SELECT 2)) s WHERE a = (SELECT 3) RETURNING WITH (OLD AS before) (SELECT 4), before.a",
        "WITH c AS (SELECT (SELECT 1)) MERGE INTO t AS target USING (SELECT (SELECT 2)) s ON (SELECT true) WHEN MATCHED AND (SELECT 3) > 0 THEN UPDATE SET a[(SELECT 4)] = (SELECT 5) WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE AND (SELECT 6) > 0 THEN DO NOTHING WHEN NOT MATCHED AND (SELECT 7) > 0 THEN INSERT (a[(SELECT 9)]) OVERRIDING SYSTEM VALUE VALUES ((SELECT 8)) RETURNING WITH (OLD AS before, NEW AS after) (SELECT 10), before.a, after.a",
        "WITH a AS (INSERT INTO t VALUES ((SELECT 1)) RETURNING x), b AS (UPDATE t SET x = (SELECT 2) RETURNING x), c AS (DELETE FROM t WHERE x = (SELECT 3) RETURNING x), d AS (MERGE INTO t USING s ON (SELECT true) WHEN MATCHED THEN DELETE RETURNING x) SELECT * FROM a, b, c, d",
    ].into_iter().map(statement));
}

#[test]
fn retained_mutation_roots_release_each_pending_body_and_field_prefix() {
    for sql in [
        "WITH c AS (SELECT (SELECT 1)) INSERT INTO t VALUES ((SELECT 2)) ON CONFLICT DO UPDATE SET x = (SELECT 3) RETURNING (SELECT 4)",
        "UPDATE t SET x = (SELECT 1) FROM (SELECT (SELECT 2)) s WHERE x = (SELECT 3) RETURNING (SELECT 4)",
        "DELETE FROM t USING (SELECT (SELECT 1)) s WHERE x = (SELECT 2) RETURNING (SELECT 3)",
        "WITH c AS (SELECT (SELECT 1)) MERGE INTO t USING (SELECT (SELECT 2)) s ON (SELECT true) WHEN MATCHED THEN UPDATE SET x = (SELECT 3) RETURNING (SELECT 4)",
    ] {
        assert_prefix_retirement(statement(sql));
    }
}

#[test]
fn retained_mutation_roots_bound_nested_cte_bodies_and_late_failure() {
    for fail in [false, true] {
        let Statement::Insert(mut source) = statement("INSERT INTO t VALUES (1) RETURNING x")
        else {
            unreachable!()
        };
        for _ in 0..512 {
            let Statement::Insert(mut parent) = statement("WITH nested AS (INSERT INTO t VALUES (1) RETURNING x) INSERT INTO t SELECT x FROM nested RETURNING x") else { unreachable!() };
            parent.with[0].body = crate::ast::CteBody::Insert(Box::new(source));
            source = parent;
        }
        if fail {
            source.returning[0].expr =
                Expr::ScalarSubquery(Box::new(blocks::query("SELECT reject_function()")));
        }
        let source = Arc::new(Statement::Insert(source));
        let retained_source = source.clone();
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let native = MemoryBudget::new(64 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let result = input(source, &native, &work, &token, &token, false)
                    .read(&mut std::io::empty());
                if fail {
                    assert_eq!(
                        result.err().unwrap().to_string(),
                        "internal error: rejected aggregate classification"
                    );
                } else {
                    let plan = result.unwrap();
                    assert_eq!(allocation_counter::measure(|| drop(plan)).count_total, 0);
                }
                assert_eq!((native.used(), work.used()), (0, 0));
            })
            .unwrap()
            .join()
            .unwrap();
        drop(retained_source);
    }
}
