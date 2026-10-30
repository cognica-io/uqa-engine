//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{Produced, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::roles::RoleReference,
    plan::{CommandPlan, QueryPlan, RelationalPlan},
    semantics::view_privileges,
};

fn command(sql: &str) -> Box<CommandPlan> {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    command
}
fn authorize(
    engine: &Engine,
    command: &CommandPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, uqa_sql::SQLError> {
    match command {
        CommandPlan::Insert(plan) => {
            view_privileges::ensure_insert_with_control(engine, plan, control)
        }
        CommandPlan::Update(plan) => {
            view_privileges::ensure_update_with_control(engine, plan, control)
        }
        CommandPlan::Delete(plan) => {
            view_privileges::ensure_delete_with_control(engine, plan, control)
        }
        CommandPlan::Merge(plan) => {
            view_privileges::ensure_merge_with_control(engine, plan, control)
        }
        _ => panic!("mutation"),
    }
}
fn arena(command: &mut CommandPlan) -> &mut Vec<QueryPlan> {
    match command {
        CommandPlan::Insert(plan) => &mut plan.subqueries,
        CommandPlan::Update(plan) => &mut plan.subqueries,
        CommandPlan::Delete(plan) => &mut plan.subqueries,
        CommandPlan::Merge(plan) => &mut plan.subqueries,
        _ => panic!("mutation"),
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_authorization_borrows_definitions_and_arenas_and_retains_the_request_allowance(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("view-privilege-owners.db"));
    engine
        .sql(
            "CREATE TABLE items(id integer PRIMARY KEY, amount integer DEFAULT 2)",
            &[],
        )
        .unwrap();
    let large = "x".repeat(4 << 20);
    engine
        .sql(
            &format!("CREATE VIEW v AS SELECT id, amount, '{large}'::text AS unused FROM items"),
            &[],
        )
        .unwrap();
    let UnifiedPlan::Query(mut unused) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("block")
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str(large));
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &invoking);
    let before = memory.used();
    for sql in [
        "INSERT INTO public.v(id) VALUES(1) RETURNING id",
        "UPDATE public.v SET amount = 7 WHERE id = 1 RETURNING id",
        "DELETE FROM public.v WHERE id = 1 RETURNING id",
        "MERGE INTO public.v AS target USING (VALUES(1)) AS source(id) ON target.id = source.id WHEN MATCHED THEN UPDATE SET amount = 9 WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id) RETURNING target.id",
    ] {
        let mut command = command(sql);
        arena(&mut command).push((*unused).clone());
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let observed = allocation_counter::measure(|| { drop(authorize(&engine, &command, &control).unwrap()); });
        assert!(observed.bytes_total < 256 << 10, "{provider:?}: {observed:?}");
        assert_eq!(memory.used(), before + occupied.bytes());
        drop(occupied);
        for token in [retained.cancellation(), &invoking] {
            token.cancel();
            assert_eq!(authorize(&engine, &command, &control).unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), before);
            token.reset();
        }
    }
    let mut wide = command("UPDATE public.v SET amount = 7 RETURNING id");
    let CommandPlan::Update(plan) = &mut *wide else {
        panic!("update")
    };
    plan.returning = vec![plan.returning[0].clone(); 8192];
    let occupied = memory.reserve(memory.available() - (16 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        assert_eq!(
            authorize(&engine, &wide, &control).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(
        observed.bytes_total < 256 << 10,
        "{provider:?}: {observed:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    drop(authorize(&engine, &wide, &control).unwrap());
    assert_eq!(memory.used(), before);
    engine
        .sql(
            "INSERT INTO v(id) VALUES(1); UPDATE v SET amount = 7 WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT amount FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(7))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_column_authorization_matches_postgresql_and_survives_reopen(
    #[case] provider: Provider,
) {
    // PostgreSQL 18.4: allowed RETURNING rows are 1, 2 and [1, 3]; DELETE and hidden-column SELECT fail 42501, unknown UPDATE fails 42703.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-column-authorization.db");
    let engine = provider.open(&path);
    engine.sql("CREATE ROLE view_reader; CREATE TABLE items(id integer PRIMARY KEY, amount integer DEFAULT 2); INSERT INTO items VALUES(1,5); CREATE VIEW v AS SELECT id,amount FROM items; GRANT SELECT(id), INSERT(id), UPDATE(amount) ON v TO view_reader; SET ROLE view_reader", &[]).unwrap();
    for (sql, expected) in [
        ("UPDATE v SET amount = 7 WHERE id = 1 RETURNING id", vec![1]),
        ("INSERT INTO v(id) VALUES(2) RETURNING id", vec![2]),
        ("MERGE INTO v AS target USING (VALUES(1),(3)) AS source(id) ON target.id = source.id WHEN MATCHED THEN UPDATE SET amount = 9 WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id) RETURNING target.id", vec![1, 3]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), expected.len());
        for (row, value) in expected.into_iter().enumerate() { assert_eq!(result.value_at(row, 0), Some(&Value::Int(value))); }
    }
    engine.sql("BEGIN", &[]).unwrap();
    for (sql, state) in [
        ("DELETE FROM v WHERE id = 2", "42501"),
        ("SELECT amount FROM v", "42501"),
        ("UPDATE v SET unknown_column = 4", "42703"),
    ] {
        engine.sql("SAVEPOINT denied", &[]).unwrap();
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some(state));
        engine.sql("ROLLBACK TO denied", &[]).unwrap();
    }
    engine.sql("COMMIT; RESET ROLE", &[]).unwrap();
    check_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        check_rows(&reopened);
        reopened.sql("SET ROLE view_reader", &[]).unwrap();
        assert_eq!(
            reopened.sql("DELETE FROM v", &[]).unwrap_err().sqlstate(),
            Some("42501")
        );
        reopened.close().unwrap();
    }
}
fn check_rows(engine: &Engine) {
    let result = engine
        .sql("SELECT id,amount FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    for (row, (id, amount)) in [(1, 9), (2, 2), (3, 2)].into_iter().enumerate() {
        assert_eq!(result.value_at(row, 0), Some(&Value::Int(id)));
        assert_eq!(result.value_at(row, 1), Some(&Value::Int(amount)));
    }
}
