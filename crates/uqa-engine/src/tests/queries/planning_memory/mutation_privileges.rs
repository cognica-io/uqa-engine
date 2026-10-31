//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    catalog::security::{table::TableAclPrivilege, view_authorization::ViewAuthorizationContext},
    plan::CommandPlan,
    semantics::{mutation_privileges, view_privileges::ViewPrivilegeCatalog},
    SQLError,
};

fn command(sql: &str) -> Box<CommandPlan> {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command");
    };
    command
}

fn insert_names(
    engine: &Engine,
    statement: &uqa_sql::plan::InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    mutation_privileges::insert_target_columns_with_control(engine, statement, control)
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_mutation_target_names_observe_only_demanded_columns(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("mutation-target-names.db"));
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let mut unused = table.columns.read()[0].clone();
    unused.name = "x".repeat(4 << 20);
    table.columns.write().push(unused);
    let statement = command("INSERT INTO public.items VALUES(1)");
    let CommandPlan::Insert(statement) = &*statement else {
        panic!("insert");
    };
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let session = CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &session);
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        selected = Some(insert_names(&engine, statement, &control).unwrap());
    });
    assert!(
        measured.bytes_total < 16 << 10,
        "{provider:?}: {measured:?}"
    );
    let selected = selected.unwrap();
    assert_eq!(selected.as_slice(), ["id"]);
    assert_eq!(
        selected.reserved_bytes(),
        usize::try_from(measured.bytes_current).unwrap()
    );
    assert_eq!(
        memory.used(),
        baseline + occupied.bytes() + selected.reserved_bytes()
    );
    drop(selected);
    assert_eq!(memory.used(), baseline + occupied.bytes());
    let merge = command("MERGE INTO public.items USING (VALUES(1)) s(id) ON items.id=s.id WHEN MATCHED THEN UPDATE SET id=2");
    let CommandPlan::Merge(merge) = &*merge else {
        panic!("merge");
    };
    let measured = allocation_counter::measure(|| {
        uqa_sql::semantics::merge::ensure_merge_mutation_privileges_with_control(
            &engine, merge, &control,
        )
        .unwrap();
    });
    assert!(
        measured.bytes_total < 16 << 10,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    for token in [retained.cancellation(), &session] {
        token.cancel();
        assert_eq!(
            insert_names(&engine, statement, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            uqa_sql::semantics::merge::ensure_merge_mutation_privileges_with_control(
                &engine, merge, &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), baseline + occupied.bytes());
        token.reset();
    }
    drop(occupied);
    table.columns.write().pop();
    drop(table);
    engine
        .sql("INSERT INTO items VALUES(1); UPDATE items SET id=2", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
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
fn native_mutation_authorization_bounds_expression_workspace_and_recovers(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("mutation-authority-workspace.db"));
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let session = CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &session);
    let baseline = memory.used();
    for sql in [
        "UPDATE public.items SET id=1 RETURNING id",
        "DELETE FROM public.items RETURNING id",
    ] {
        let mut input = command(sql);
        match &mut *input {
            CommandPlan::Update(plan) => plan.returning = vec![plan.returning[0].clone(); 8192],
            CommandPlan::Delete(plan) => plan.returning = vec![plan.returning[0].clone(); 8192],
            _ => panic!("mutation"),
        }
        let authorize = || match &*input {
            CommandPlan::Update(plan) => {
                mutation_privileges::ensure_update_target_privileges_with_control(
                    &engine, plan, &control,
                )
            }
            CommandPlan::Delete(plan) => {
                mutation_privileges::ensure_delete_target_privileges_with_control(
                    &engine, plan, &control,
                )
            }
            _ => panic!("mutation"),
        };
        let occupied = memory.reserve(memory.available() - (16 << 10)).unwrap();
        let measured = allocation_counter::measure(|| {
            assert_eq!(authorize().unwrap_err().sqlstate(), Some("53200"));
        });
        assert!(
            measured.bytes_total < 64 << 10,
            "{provider:?}: {measured:?}"
        );
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        drop(authorize().unwrap());
        assert_eq!(memory.used(), baseline);
    }
    engine
        .sql(
            "INSERT INTO items VALUES(1); UPDATE items SET id=2; DELETE FROM items",
            &[],
        )
        .unwrap();
    assert!(engine
        .sql("SELECT id FROM items", &[])
        .unwrap()
        .rows
        .is_empty());
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_authorization_borrows_role_guards_without_allocation(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("view-role-guards.db"));
    engine.sql("CREATE VIEW v AS SELECT 1 AS id", &[]).unwrap();
    let view = ViewPrivilegeCatalog::view_definition(&engine, "public.v")
        .unwrap()
        .unwrap();
    let subject = engine.current_role();
    let memory = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    let context = ViewAuthorizationContext { roles: &engine };
    let measured = allocation_counter::measure(|| {
        context
            .ensure_view_privilege_for_with_control(
                "public.v",
                &view,
                &subject,
                TableAclPrivilege::Select,
                &control,
            )
            .unwrap();
        context
            .ensure_view_column_privilege_for_with_control(
                "public.v",
                &view,
                "id",
                &subject,
                TableAclPrivilege::Select,
                &control,
            )
            .unwrap();
        context
            .ensure_any_view_column_privilege_for_with_control(
                "public.v",
                &view,
                &subject,
                TableAclPrivilege::Select,
                &control,
            )
            .unwrap();
    });
    assert_eq!(measured.bytes_total, 0, "{provider:?}: {measured:?}");
    assert_eq!(memory.used(), 0);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_table_mutation_authorization_matches_postgresql_and_survives_reopen(
    #[case] provider: Provider,
) {
    // PostgreSQL 18.4: column grants permit these RETURNING rows; denials preserve 42501/42703 and savepoint recovery.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("table-column-authorization.db");
    let engine = provider.open(&path);
    engine.sql("CREATE ROLE planning_writer; CREATE TABLE items(id integer PRIMARY KEY DEFAULT 7, amount integer DEFAULT 2); INSERT INTO items VALUES(1,5); GRANT SELECT(id),INSERT(id),UPDATE(amount) ON items TO planning_writer; SET ROLE planning_writer", &[]).unwrap();
    for (sql, expected) in [
        ("UPDATE items SET amount=7 WHERE id=1 RETURNING id", vec![1]),
        ("INSERT INTO items(id) VALUES(2) RETURNING id", vec![2]),
        ("INSERT INTO items(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET amount=excluded.id+10 RETURNING id", vec![1]),
        ("MERGE INTO items target USING (VALUES(1),(3)) source(id) ON target.id=source.id WHEN MATCHED THEN UPDATE SET amount=9 WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id) RETURNING target.id", vec![1,3]),
        ("INSERT INTO items DEFAULT VALUES RETURNING id", vec![7]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), expected.len());
        for (row, value) in expected.into_iter().enumerate() { assert_eq!(result.value_at(row,0), Some(&Value::Int(value))); }
    }
    engine.sql("EXPLAIN (COSTS OFF) INSERT INTO items(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET amount=8 RETURNING id; BEGIN", &[]).unwrap();
    for (sql, state) in [
        ("DELETE FROM items WHERE id=2", "42501"),
        ("SELECT amount FROM items", "42501"),
        ("UPDATE items SET unknown_column=4", "42703"),
        (
            "INSERT INTO items(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET amount=items.amount+1",
            "42501",
        ),
    ] {
        engine.sql("SAVEPOINT denied", &[]).unwrap();
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some(state));
        engine.sql("ROLLBACK TO denied", &[]).unwrap();
    }
    engine.sql("COMMIT; RESET ROLE", &[]).unwrap();
    check_table_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        check_table_rows(&engine);
        engine.sql("SET ROLE planning_writer", &[]).unwrap();
        assert_eq!(
            engine.sql("DELETE FROM items", &[]).unwrap_err().sqlstate(),
            Some("42501")
        );
        engine.close().unwrap();
    }
}

fn check_table_rows(engine: &Engine) {
    let result = engine
        .sql("SELECT id,amount FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 4);
    for (row, (id, amount)) in [(1, 9), (2, 2), (3, 2), (7, 2)].into_iter().enumerate() {
        assert_eq!(result.value_at(row, 0), Some(&Value::Int(id)));
        assert_eq!(result.value_at(row, 1), Some(&Value::Int(amount)));
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_explain_mutation_authorization_admits_its_private_command_copy(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-mutation-owner.db"));
    engine.sql("CREATE ROLE planning_writer; CREATE TABLE items(id integer); GRANT SELECT,DELETE ON items TO planning_writer; REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC; SET ROLE planning_writer", &[]).unwrap();
    let mut input = command("DELETE FROM public.items RETURNING id");
    let CommandPlan::Delete(plan) = &mut *input else {
        panic!("delete");
    };
    let UnifiedPlan::Query(mut unused) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("query block");
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    plan.subqueries.push(*unused);
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    {
        let context = engine.mutation_statement_context();
        assert!(!uqa_execution::catalog::security::builtin_routines::execution::BuiltinRoutinePermissions::capture(&context.query.source.catalog).unrestricted());
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut result = None;
        let observed = allocation_counter::measure(|| {
            result = Some(uqa_execution::mutation::routine_calls::explain(
                &context,
                &engine,
                &input,
                &[],
            ));
        });
        assert!(
            observed.bytes_total < 128 << 10,
            "{provider:?}: {observed:?}"
        );
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        for token in [
            retained.cancellation(),
            context.mutation.scopes.cancellation(),
        ] {
            token.cancel();
            assert_eq!(
                uqa_execution::mutation::routine_calls::explain(&context, &engine, &input, &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), baseline);
            token.reset();
        }
    }
    engine.sql("EXPLAIN DELETE FROM items RETURNING id; RESET ROLE; INSERT INTO items VALUES(1); DELETE FROM items", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
