//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_sql::{
    ast::Expr,
    semantics::rules::{
        action_binding::{bind_rule_action, bind_rule_action_with_control},
        binding::RuleQueryResolver,
    },
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_binding_uses_the_original_allowance_and_releases_destinations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-binding.db"));
    engine
        .sql(
            "CREATE TABLE items (id integer, payload text); INSERT INTO items VALUES (1, 'kept')",
            &[],
        )
        .unwrap();
    let action = uqa_sql::compile("SELECT new.payload FROM public.items WHERE items.id > 0")
        .unwrap()
        .remove(0);
    let columns = BTreeSet::new();
    let mut resolver = RuleQueryResolver {
        table: "public.items".into(),
        columns: vec![(
            "payload".into(),
            Ok(Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)))),
        )],
        old: std::collections::BTreeMap::new(),
        expressions: std::collections::BTreeMap::new(),
    };
    let ordinary = allocation_counter::measure(|| {
        drop(bind_rule_action(&engine, &action, &columns, &mut resolver).unwrap());
    });
    assert!(ordinary.bytes_total >= 4 * 1024 * 1024, "{provider:?}");
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 64 * 1024).unwrap();
    let observed = allocation_counter::measure(|| {
        let error =
            bind_rule_action_with_control(&engine, &action, &columns, &mut resolver, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}");
    });
    assert!(
        observed.bytes_total < 128 * 1024,
        "{provider:?}: {observed:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    resolver.columns[0].1 = Ok(Expr::Literal(Value::Str("ok".into())));
    let result =
        bind_rule_action_with_control(&engine, &action, &columns, &mut resolver, &control).unwrap();
    assert!(result.reserved_bytes() > 0);
    assert_eq!(
        memory.used(),
        before + occupied.bytes() + result.reserved_bytes()
    );
    drop(result);
    for token in [retained.cancellation(), &invoking] {
        token.cancel();
        assert_eq!(
            bind_rule_action_with_control(&engine, &action, &columns, &mut resolver, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), before + occupied.bytes());
        token.reset();
    }
    returning_replacement(&engine, &control);
    drop(occupied);
    assert_eq!(memory.used(), before);
    assert_eq!(
        engine
            .sql("SELECT payload FROM items WHERE id = 1", &[])
            .unwrap()
            .rows[0]
            .get("payload"),
        Some(&Value::Str("kept".into()))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

fn returning_replacement(engine: &Engine, control: &ProductionControl<'_>) {
    use uqa_sql::{
        ast::ReturningAliases,
        plan::ProjectionPlan,
        semantics::rules::{action_binding::BoundRuleStatement, returning::RuleReturningRequest},
        ScalarExpr,
    };
    let syntax = uqa_sql::compile("UPDATE items SET id = 3 RETURNING payload")
        .unwrap()
        .remove(0);
    let columns = BTreeSet::from(["id".into(), "payload".into()]);
    let mut resolver = RuleQueryResolver {
        table: "public.items".into(),
        columns: Vec::new(),
        old: std::collections::BTreeMap::new(),
        expressions: std::collections::BTreeMap::new(),
    };
    let memory = control.budget().unwrap();
    let baseline = memory.used();
    let mut bound =
        BoundRuleStatement::bind(engine, &syntax, &columns, &mut resolver, control).unwrap();
    let request = RuleReturningRequest::from_plan_with_control(
        &[ProjectionPlan {
            expr: ScalarExpr::Column("payload".into()),
            alias: None,
        }],
        &ReturningAliases::default(),
        &[],
        control,
    )
    .unwrap();
    let payload = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let before = memory.used();
    let observed = allocation_counter::measure(|| {
        assert_eq!(
            bound
                .augment_returning(Some(&payload), 1, request, &columns)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(observed.bytes_total < 16 * 1024, "{observed:?}");
    assert_eq!(observed.bytes_current, 0);
    assert_eq!(memory.used(), before);
    let uqa_sql::ast::Statement::Update(update) = bound.statement() else {
        panic!("mutation owner");
    };
    assert_eq!(update.returning.len(), 1);
    assert_eq!(update.returning[0].expr, Expr::Column("payload".into()));
    bound.augment_returning(None, 1, request, &columns).unwrap();
    assert_eq!(memory.used(), baseline + bound.reserved_bytes());
    bound.clear_returning().unwrap();
    drop(bound);
    assert_eq!(memory.used(), baseline);
}
