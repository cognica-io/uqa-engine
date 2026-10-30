//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Projection, plan::ProjectionPlan, semantics::rules::returning::RuleReturningRequest,
    ScalarExpr,
};

fn request() -> RuleReturningRequest {
    RuleReturningRequest::from_plan(
        &[
            ProjectionPlan {
                expr: ScalarExpr::Column("n".into()),
                alias: None,
            },
            ProjectionPlan {
                expr: ScalarExpr::QualifiedColumn {
                    qualifier: "old".into(),
                    column: "n".into(),
                },
                alias: None,
            },
            ProjectionPlan {
                expr: ScalarExpr::QualifiedColumn {
                    qualifier: "new".into(),
                    column: "n".into(),
                },
                alias: None,
            },
        ],
        &crate::ast::ReturningAliases::default(),
        &[],
    )
}

fn projections(value: &BoundRuleStatement) -> &[Projection] {
    match value.statement() {
        Statement::Insert(value) => &value.returning,
        Statement::Update(value) => &value.returning,
        Statement::Delete(value) => &value.returning,
        _ => panic!("expected DML"),
    }
}

#[test]
fn replacing_returning_keeps_only_live_clause_allocations_and_preserves_the_body() {
    let columns = BTreeSet::from(["n".into(), "a".into()]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(1024 * 1024);
    let sibling = memory.reserve(73).unwrap();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let request = request();
    for (sql, old, new) in [
        (
            "INSERT INTO items(n) VALUES(7) RETURNING n AS kept",
            false,
            true,
        ),
        ("UPDATE items SET n=7 RETURNING n AS kept", true, true),
        ("DELETE FROM items RETURNING n AS kept", true, false),
    ] {
        let syntax = statement(sql);
        let mut bound = BoundRuleStatement::bind(
            &Catalog,
            &syntax,
            &columns,
            &mut Resolver::default(),
            &control,
        )
        .unwrap();
        let (body_pointer, body_capacity) = match bound.statement() {
            Statement::Insert(value) => (value.table.as_ptr(), value.table.capacity()),
            Statement::Update(value) => (value.table.as_ptr(), value.table.capacity()),
            Statement::Delete(value) => (value.table.as_ptr(), value.table.capacity()),
            _ => unreachable!(),
        };
        let before = memory.used();
        let allocation = allocation_counter::measure(|| {
            bound
                .augment_returning(Some(&Expr::Literal(Value::Int(9))), 1, request, &columns)
                .unwrap();
        });
        assert_eq!(
            allocation.bytes_current,
            memory.used() as i64 - before as i64
        );
        assert_eq!(memory.used(), sibling.bytes() + bound.reserved_bytes());
        let values = projections(&bound);
        assert_eq!(values.len(), 4);
        assert_eq!(values[0].expr, Expr::Column("n".into()));
        for (index, image, selected) in [(1, "old", old), (2, "new", new)] {
            assert_eq!(
                values[index].expr,
                if selected {
                    Expr::qualified_column(image, "n")
                } else {
                    Expr::Literal(Value::Null)
                }
            );
            assert_eq!(values[index].alias.as_deref(), selected.then_some("kept"));
        }
        assert_eq!(values[3].expr, Expr::Literal(Value::Int(9)));
        if !matches!(bound.statement(), Statement::Insert(_)) {
            let context = [Expr::Literal(Value::Str("context".into()))];
            let before = memory.used();
            let allocation =
                allocation_counter::measure(|| bound.append_returning_context(&context).unwrap());
            assert_eq!(
                allocation.bytes_current,
                memory.used() as i64 - before as i64
            );
            assert_eq!(projections(&bound).last().unwrap().expr, context[0]);
        }
        let before = memory.used();
        let allocation = allocation_counter::measure(|| bound.clear_returning().unwrap());
        assert!(allocation.bytes_current < 0);
        assert_eq!(
            allocation.bytes_current,
            memory.used() as i64 - before as i64
        );
        assert!(projections(&bound).is_empty());
        let table = match bound.statement() {
            Statement::Insert(value) => &value.table,
            Statement::Update(value) => &value.table,
            Statement::Delete(value) => &value.table,
            _ => unreachable!(),
        };
        assert_eq!(table.as_ptr(), body_pointer);
        assert_eq!(table.capacity(), body_capacity);
        let completed = bound.into_produced().unwrap();
        assert_eq!(memory.used(), sibling.bytes() + completed.reserved_bytes());
        drop(completed);
        assert_eq!(memory.used(), sibling.bytes());
    }
}

#[test]
fn rejected_returning_replacements_preserve_the_original_owner_and_recover() {
    let syntax = statement("UPDATE items SET n=7 RETURNING n");
    let columns = BTreeSet::from(["n".into()]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(128 * 1024);
    let sibling = memory.reserve(47).unwrap();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut bound = BoundRuleStatement::bind(
        &Catalog,
        &syntax,
        &columns,
        &mut Resolver::default(),
        &control,
    )
    .unwrap();
    let saved = serde_json::to_value(bound.statement()).unwrap();
    let before = memory.used();
    let expressions = [
        Expr::Literal(Value::Str("small".into())),
        Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024))),
    ];
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            bound
                .append_returning_context(&expressions)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 16 * 1024);
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(memory.used(), before);
    assert_eq!(serde_json::to_value(bound.statement()).unwrap(), saved);
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        bound
            .augment_returning(None, 1, request(), &columns)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(serde_json::to_value(bound.statement()).unwrap(), saved);
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            bound.clear_returning().unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(
            bound
                .augment_returning(None, 1, request(), &columns)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            bound.append_returning_context(&[]).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(serde_json::to_value(bound.statement()).unwrap(), saved);
        assert_eq!(memory.used(), before);
        token.reset();
    }
    bound
        .augment_returning(None, 1, request(), &columns)
        .unwrap();
    drop(bound);
    assert_eq!(memory.used(), sibling.bytes());
}
