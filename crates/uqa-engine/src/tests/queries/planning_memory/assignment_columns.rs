//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, RelationIdentity};
use uqa_sql::{
    assignment::columns::{
        declared_column_names_with_control, validate_mutation_targets_with_control,
        AssignmentColumnCatalog,
    },
    semantics::generated_values::GeneratedValueColumns,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_assignment_metadata_borrows_unused_defaults_and_owns_selected_names(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("assignment-declarations.db"));
    engine
        .sql(
            "CREATE TABLE items(id integer GENERATED ALWAYS AS IDENTITY, payload text)",
            &[],
        )
        .unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    drop(GeneratedValueColumns::of_with_control(&engine, "public.items", &control).unwrap());
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    table.columns.write()[1].default =
        Some(uqa_sql::ast::Expr::Literal(Value::Str("x".repeat(4 << 20))));
    let targets: [uqa_sql::ast::AssignmentTarget<uqa_sql::ScalarExpr>; 2] =
        ["id".into(), "payload".into()];
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        validate_mutation_targets_with_control(
            &engine,
            "public.items",
            &targets,
            "INSERT",
            true,
            &control,
        )
        .unwrap();
        let generated =
            GeneratedValueColumns::of_with_control(&engine, "public.items", &control).unwrap();
        generated
            .validate_insert_with_control([("id", false), ("payload", true)], None, &control)
            .unwrap();
        generated
            .validate_update_with_control([("payload", false)], &control)
            .unwrap();
        let names = declared_column_names_with_control(&engine, "public.items", &control)
            .unwrap()
            .unwrap();
        result = Some((generated, names));
    });
    assert!(
        measured.bytes_total < 128 << 10,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(
        memory.used() - baseline - occupied.bytes(),
        usize::try_from(measured.bytes_current).unwrap(),
        "{provider:?}: {measured:?}"
    );
    let (generated, names) = result.unwrap();
    assert_eq!(&**names, &["id", "payload"]);
    assert_eq!(
        generated
            .validate_insert_with_control([("id", true)], None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("428C9")
    );
    drop(names);
    drop(generated);
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        let error = GeneratedValueColumns::of_with_control(&engine, "public.items", &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        token.reset();
        assert_eq!(memory.used(), baseline);
    }
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = GeneratedValueColumns::of_with_control(&engine, "public.items", &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    table.columns.write()[1].default = None;
    assert!(AssignmentColumnCatalog::with_column_metadata_with_control(
        &engine,
        "public.items",
        &control,
        &mut |columns| {
            assert!(!table.columns.is_locked());
            table.columns.write()[1].name = "changed".into();
            assert_eq!(columns[1].name, "payload");
            Ok(())
        }
    )
    .unwrap());
    table.columns.write()[1].name = "payload".into();
    drop(table);
    assert_eq!(
        engine
            .sql("INSERT INTO items(payload) VALUES('ok') RETURNING id", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
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
fn native_assignment_metadata_preserves_sequence_observation_before_validation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("assignment-sequences.db"));
    engine.sql("CREATE SEQUENCE original_seq; CREATE TABLE items(id integer, n bigint DEFAULT nextval('original_seq')); ALTER SEQUENCE original_seq RENAME TO renamed_seq", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let targets: [uqa_sql::ast::AssignmentTarget<uqa_sql::ScalarExpr>; 1] = ["id".into()];
    validate_mutation_targets_with_control(
        &engine,
        "public.items",
        &targets,
        "UPDATE",
        false,
        &control,
    )
    .unwrap();
    let baseline = memory.used();
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let original = table.columns.read()[1].default.clone();
    let uqa_sql::Statement::CreateTable(mut missing) =
        uqa_sql::compile("CREATE TABLE t(n bigint DEFAULT nextval('missing_sequence'))")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.columns.write()[1].default = missing.columns.remove(0).default;
    let ordinary = engine
        .describe_table_in_execution("public.items")
        .unwrap_err();
    let error = validate_mutation_targets_with_control(
        &engine,
        "public.items",
        &targets,
        "UPDATE",
        false,
        &control,
    )
    .unwrap_err();
    let uqa_sql::SQLError::Internal(message) = error else {
        panic!("the stored-reference error keeps its storage context")
    };
    assert_eq!(
        message,
        format!("UPDATE failed in storage backend: {ordinary}")
    );
    assert_eq!(memory.used(), baseline);
    table.columns.write()[1].default = original;
    drop(table);
    assert_eq!(
        engine
            .sql("INSERT INTO items(id) VALUES(1) RETURNING n", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
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
fn native_generated_assignment_validation_preserves_postgresql_effects_and_reopen(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("assignment-effects.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload integer, computed integer GENERATED ALWAYS AS(payload+1) STORED); CREATE SEQUENCE ignored_values", &[]).unwrap();
    for (sql, column, update) in [
        ("INSERT INTO items(id,payload) SELECT 50,3 WHERE false", "id", false),
        ("UPDATE items SET id=50 WHERE false", "id", true),
        ("MERGE INTO items USING (SELECT 50 AS n WHERE false) source ON false WHEN NOT MATCHED THEN INSERT(id,payload) VALUES(source.n,3)", "id", false),
        ("INSERT INTO items(computed,id) VALUES(8,50)", "id", false),
        ("INSERT INTO items(computed) OVERRIDING SYSTEM VALUE VALUES(8)", "computed", false),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("428C9"), "{provider:?}: {sql}: {error}");
        assert_eq!(error.to_string(), if update { format!("column \"{column}\" can only be updated to DEFAULT") } else { format!("cannot insert a non-DEFAULT value into column \"{column}\"") });
        assert_eq!(error.detail(), Some(if column=="id" { "Column \"id\" is an identity column defined as GENERATED ALWAYS." } else { "Column \"computed\" is a generated column." }));
        assert_eq!(error.hint(), if !update && column=="id" { Some("Use OVERRIDING SYSTEM VALUE to override.") } else { None });
    }
    engine.sql("INSERT INTO items(id,payload) OVERRIDING USER VALUE VALUES(nextval('ignored_values'),10); INSERT INTO items(id,payload) OVERRIDING USER VALUE SELECT nextval('ignored_values'),20; MERGE INTO items USING (VALUES(30)) source(n) ON false WHEN NOT MATCHED THEN INSERT(id,payload) OVERRIDING USER VALUE VALUES(nextval('ignored_values'),source.n)", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT currval('ignored_values')", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    engine.sql("PREPARE saved AS UPDATE items SET payload=payload+1 WHERE id=1 RETURNING id,payload,computed; SET plan_cache_mode=force_generic_plan; EXECUTE saved; SET plan_cache_mode=force_custom_plan; EXECUTE saved", &[]).unwrap();
    engine.sql("BEGIN; SAVEPOINT before_change; UPDATE items SET payload=90 WHERE id=2; ROLLBACK TO before_change; COMMIT", &[]).unwrap();
    let expected = [[1, 12, 13], [2, 20, 21], [3, 30, 31]]
        .map(|row| {
            std::collections::BTreeMap::from([
                ("id".into(), Value::Int(row[0])),
                ("payload".into(), Value::Int(row[1])),
                ("computed".into(), Value::Int(row[2])),
            ])
        })
        .to_vec();
    assert_eq!(
        engine
            .sql("SELECT id,payload,computed FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        expected
    );
    engine.sql("EXPLAIN (MEMORY, FORMAT JSON) UPDATE items SET payload=50 WHERE id=1; DEALLOCATE saved", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id,payload,computed FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        expected
    );
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_eq!(
            reopened
                .sql("SELECT id,payload,computed FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            expected
        );
        reopened.close().unwrap();
    }
}
