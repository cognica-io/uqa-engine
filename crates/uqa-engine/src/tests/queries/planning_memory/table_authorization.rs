//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::MemoryBudget, memory::ProductionControl, CancellationToken, RelationIdentity,
};
use uqa_sql::catalog::security::table::TableAclPrivilege;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_table_authorization_borrows_large_acl_generations(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("table-authority-owner.db"));
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    table
        .security
        .write()
        .column_acls
        .insert("x".repeat(4 << 20), Vec::new());
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let subject = engine.current_role();
    let measured = allocation_counter::measure(|| {
        engine
            .ensure_table_privilege_for("public.items", &subject, TableAclPrivilege::Select)
            .unwrap();
        engine
            .ensure_column_privilege_for("public.items", "id", &subject, TableAclPrivilege::Update)
            .unwrap();
        engine
            .ensure_any_column_privilege_for("public.items", &subject, TableAclPrivilege::Insert)
            .unwrap();
    });
    assert!(
        measured.bytes_total < 16 << 10,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    selected_columns(&engine, &table, memory);
    drop(occupied);
    table.security.write().column_acls.clear();
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
    drop(table);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

fn selected_columns(engine: &Engine, table: &crate::TableState, memory: &MemoryBudget) {
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(memory, &statement, &session);
    let context = engine.table_authorization_context();
    let before = memory.used();
    let mut names = None;
    let measured = allocation_counter::measure(|| {
        names = Some(
            context
                .bound_table_column_names_with_control("public.items", &control)
                .unwrap(),
        );
    });
    let names = names.unwrap();
    assert_eq!(names.as_slice(), &["id"]);
    assert_eq!(
        memory.used() - before,
        usize::try_from(measured.bytes_current).unwrap()
    );
    assert_eq!(names.reserved_bytes(), memory.used() - before);
    drop(names);
    assert_eq!(memory.used(), before);
    table.columns.write()[0].name = "x".repeat(4 << 20);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            context
                .bound_table_column_names_with_control("public.items", &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(memory.used(), before);
    table.columns.write()[0].name = "id".into();
    for cancellation in [&statement, &session] {
        cancellation.cancel();
        assert_eq!(
            context
                .bound_table_column_names_with_control("public.items", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), before);
        cancellation.reset();
    }
    drop(
        context
            .bound_table_column_names_with_control("public.items", &control)
            .unwrap(),
    );
    assert_eq!(memory.used(), before);
}
