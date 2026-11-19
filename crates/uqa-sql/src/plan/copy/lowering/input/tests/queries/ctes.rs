//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::CteBody;

fn recursive_query() -> SelectStmt {
    blocks::query("WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 3) SEARCH DEPTH FIRST BY n SET seq CYCLE n SET mark TO 1 DEFAULT 0 USING path SELECT * FROM t")
}

#[test]
fn retained_query_ctes_preserve_cycle_scopes_and_each_suspended_prefix() {
    let mut source = recursive_query();
    let cycle = source.with[0].cycle.as_mut().unwrap();
    cycle.mark_value = Expr::ScalarSubquery(Box::new(blocks::query("SELECT NOT ((SELECT 1) = 2)")));
    cycle.mark_default = Expr::ScalarSubquery(Box::new(blocks::query("SELECT (SELECT 3)")));
    let source = Statement::Select(Box::new(source));
    assert_equivalent([source.clone()]);
    assert_prefix_retirement(source);
}

#[test]
fn retained_query_ctes_retire_deep_cycle_after_body_or_sibling_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for failure in 0..3 {
                let mut source = recursive_query();
                let cycle = source.with[0].cycle.as_mut().unwrap();
                let mut mark = Expr::Literal(Value::Bool(true));
                for _ in 0..8192 {
                    mark = Expr::Not(Box::new(mark));
                }
                cycle.mark_value = mark;
                let expected = match failure {
                    0 => {
                        let mut body = blocks::query("SELECT 1");
                        body.projections[0].expr = blocks::query("SELECT reject_function()")
                            .projections
                            .remove(0)
                            .expr;
                        source.with[0].body = CteBody::Query(Box::new(body));
                        "internal error: rejected aggregate classification"
                    }
                    1 => {
                        source.with[0].body = blocks::query(
                            "WITH t AS (DELETE FROM dst RETURNING x) SELECT * FROM t",
                        )
                        .with
                        .remove(0)
                        .body;
                        let CteBody::Delete(body) = &mut source.with[0].body else {
                            unreachable!()
                        };
                        body.returning[0].expr = Expr::ScalarSubquery(Box::new(blocks::query(
                            "SELECT reject_function()",
                        )));
                        "internal error: rejected aggregate classification"
                    }
                    _ => {
                        let mut sibling = blocks::query("WITH t AS (SELECT 1) SELECT * FROM t")
                            .with
                            .remove(0);
                        sibling.body =
                            CteBody::Query(Box::new(blocks::query("SELECT reject_function()")));
                        source.with.push(sibling);
                        "internal error: rejected aggregate classification"
                    }
                };
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let sibling = native.reserve(37).unwrap();
                let error = input(
                    Arc::new(Statement::Select(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                    false,
                )
                .read(&mut std::io::empty())
                .err()
                .unwrap();
                assert_eq!(error.to_string(), expected);
                assert_eq!((native.used(), work.used()), (37, 0));
                drop(sibling);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
