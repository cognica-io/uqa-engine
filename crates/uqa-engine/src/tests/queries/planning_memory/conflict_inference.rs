//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::mutation::{insert::table::run_table_insert, insert::table::InsertPlanning};
use uqa_sql::plan::{CommandPlan, InsertPlan, RelationalPlan};

fn insert(sql: &str) -> Box<InsertPlan> {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command");
    };
    let CommandPlan::Insert(insert) = *command else {
        panic!("insert");
    };
    insert
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_conflict_inference_admits_the_complete_private_command(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("conflict-inference-owner.db"));
    engine.sql("CREATE TABLE items(id integer); CREATE UNIQUE INDEX positive_id ON items(id) WHERE id>0", &[]).unwrap();
    let sql =
        "INSERT INTO public.items VALUES(1) ON CONFLICT(id) WHERE id>0 DO NOTHING RETURNING id";
    let mut statement = insert(sql);
    let UnifiedPlan::Query(mut unused) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("block");
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    statement.subqueries.push(*unused);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    {
        let context = engine.mutation_statement_context();
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut outcome = None;
        let measured = allocation_counter::measure(|| {
            outcome = Some(
                run_table_insert(
                    &context,
                    InsertPlanning {
                        inference: engine.inference_context(),
                        returning: engine.returning_analysis_context(),
                        prune_source_outputs:
                            uqa_planner::mutation_outputs::prune_unused_query_outputs_owned,
                    },
                    &statement,
                    &[],
                    None,
                )
                .map(drop),
            );
        });
        assert!(
            measured.bytes_total < 256 << 10,
            "{provider:?}: {measured:?}"
        );
        assert_eq!(outcome.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
    }
    let rows = engine.sql(sql, &[]).unwrap();
    assert_eq!(rows.value_at(0, 0), Some(&Value::Int(1)));
    drop(rows);
    assert!(engine.sql(sql, &[]).unwrap().rows.is_empty());
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_conflict_metadata_borrows_defaults_and_releases_catalog_guards(
    #[case] provider: Provider,
) {
    use uqa_core::{memory::ProductionControl, RelationIdentity};
    use uqa_sql::semantics::conflict::{prepare_inference_predicate_with_control, ConflictCatalog};
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("conflict-metadata-owner.db"));
    engine
        .sql("CREATE TABLE items(id integer, payload text)", &[])
        .unwrap();
    let statement =
        insert("INSERT INTO public.items VALUES(1,'x') ON CONFLICT(id) WHERE id>0 DO NOTHING");
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    drop(
        prepare_inference_predicate_with_control(
            engine.inference_context(),
            &statement,
            &[],
            &control,
        )
        .unwrap(),
    );
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    table.columns.write()[1].default =
        Some(uqa_sql::ast::Expr::Literal(Value::Str("x".repeat(4 << 20))));
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            prepare_inference_predicate_with_control(
                engine.inference_context(),
                &statement,
                &[],
                &control,
            )
            .unwrap()
            .unwrap(),
        );
    });
    assert!(
        measured.bytes_total < 128 << 10,
        "{provider:?}: {measured:?}"
    );
    let output = output.unwrap();
    assert_eq!(
        output.reserved_bytes(),
        usize::try_from(measured.bytes_current).unwrap(),
        "{provider:?}: {measured:?}"
    );
    assert_eq!(
        memory.used(),
        baseline + occupied.bytes() + output.reserved_bytes()
    );
    drop(output);
    assert_eq!(memory.used(), baseline + occupied.bytes());
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            prepare_inference_predicate_with_control(
                engine.inference_context(),
                &statement,
                &[],
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), baseline + occupied.bytes());
        token.reset();
    }
    drop(occupied);
    table.columns.write()[1].default = None;
    assert!(engine
        .with_column_metadata_with_control("public.items", &control, &mut |columns| {
            assert!(!table.columns.is_locked());
            table.columns.write()[1].name = "changed".into();
            assert_eq!(columns[1].name, "payload");
            Ok(())
        })
        .unwrap());
    table.columns.write()[1].name = "payload".into();
    drop(table);
    engine
        .sql("INSERT INTO items VALUES(1,'x'); SELECT * FROM items", &[])
        .unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_conflict_metadata_preserves_sequence_dependency_observation(#[case] provider: Provider) {
    use uqa_core::{memory::ProductionControl, RelationIdentity};
    use uqa_sql::semantics::conflict::ConflictCatalog;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("conflict-sequence-observation.db"));
    engine.sql("CREATE SEQUENCE inference_seq; CREATE TABLE items(id integer, n bigint DEFAULT nextval('inference_seq')); ALTER SEQUENCE inference_seq RENAME TO renamed_seq", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let baseline = memory.used();
    assert!(engine
        .with_column_metadata_with_control("public.items", &control, &mut |columns| {
            assert_eq!(columns.len(), 2);
            Ok(())
        })
        .unwrap());
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
    let controlled = engine
        .with_column_metadata_with_control("public.items", &control, &mut |_| {
            panic!("dependency error precedes metadata use")
        })
        .unwrap_err();
    assert_eq!(
        controlled.to_string(),
        uqa_sql::SQLError::Internal(ordinary.to_string()).to_string()
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
fn native_conflict_inference_preserves_postgresql_binding_effects_and_reopen(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("conflict-inference-reference.db");
    let engine = provider.open(&path);
    engine.sql("CREATE SEQUENCE inference_sequence; CREATE TABLE conflict_sources(email text COLLATE \"C\",tenant integer,active boolean,n integer,marker bigint DEFAULT nextval('inference_sequence')); CREATE UNIQUE INDEX folded_active ON conflict_sources(lower(email),tenant) WHERE active", &[]).unwrap();
    for sql in [
        "INSERT INTO conflict_sources(email,tenant,active,n) VALUES('One',1,true,3) RETURNING email,n,marker",
        "INSERT INTO conflict_sources AS target(email,tenant,active,n) VALUES('ONE',1,true,5) ON CONFLICT(lower(email),tenant) WHERE active AND tenant>0 DO UPDATE SET n=excluded.n RETURNING email,n,marker",
        "INSERT INTO conflict_sources(email,tenant,active,n) VALUES('Two',2,true,7) ON CONFLICT(lower(email),tenant) WHERE active DO NOTHING RETURNING email,n,marker",
        "EXPLAIN(COSTS OFF) INSERT INTO conflict_sources(email,tenant,active,n) VALUES('ONE',1,true,9) ON CONFLICT(lower(email),tenant) WHERE active DO NOTHING",
    ] { engine.sql(sql, &[]).unwrap(); }
    assert_eq!(
        engine
            .sql("SELECT last_value FROM inference_sequence", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(3))
    );
    engine.sql("PREPARE inferred(text,integer) AS INSERT INTO conflict_sources(email,tenant,active,n) VALUES($1,$2,true,4) ON CONFLICT(lower(email),tenant) WHERE active DO UPDATE SET n=excluded.n RETURNING email,n,marker; EXECUTE inferred('TWO',2); CREATE VIEW inference_view AS SELECT email AS address,tenant AS account,active,n FROM conflict_sources; INSERT INTO inference_view(address,account,active,n) VALUES('ONE',1,true,8) ON CONFLICT((account::integer),lower(address)) WHERE active DO UPDATE SET n=excluded.n RETURNING address,n", &[]).unwrap();
    engine.sql("BEGIN; SAVEPOINT rejected", &[]).unwrap();
    for (target, predicate, state) in [
        ("upper(email),tenant", " WHERE active", "42P10"),
        ("lower(email),tenant", "", "42P10"),
        ("lower(email),tenant", " WHERE (SELECT true)", "0A000"),
        ("lower(email),tenant", " WHERE sum(tenant)>0", "42803"),
        (
            "lower(email),tenant",
            " WHERE row_number() OVER()>0",
            "42P20",
        ),
        ("lower(email),tenant", " WHERE missing>0", "42703"),
    ] {
        let sql = format!("INSERT INTO conflict_sources(email,tenant,active,n) VALUES('x',3,true,1) ON CONFLICT({target}){predicate} DO NOTHING");
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(state),
            "{provider:?}: {sql}: {error}"
        );
        engine.sql("ROLLBACK TO rejected", &[]).unwrap();
    }
    engine.sql("COMMIT; ALTER SEQUENCE inference_sequence RENAME TO renamed_inference_sequence; EXPLAIN(COSTS OFF) INSERT INTO conflict_sources(email,tenant,active,n) VALUES('THREE',3,true,6) ON CONFLICT(lower(email),tenant) WHERE active DO NOTHING", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT last_value FROM renamed_inference_sequence", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(5))
    );
    engine.sql("INSERT INTO conflict_sources(email,tenant,active,n) VALUES('Three',3,true,6) ON CONFLICT(lower(email),tenant) WHERE active DO NOTHING RETURNING email,n,marker; DEALLOCATE inferred", &[]).unwrap();
    check_conflict_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        check_conflict_rows(&engine);
        engine.close().unwrap();
    }
}

fn check_conflict_rows(engine: &Engine) {
    let rows = engine
        .sql(
            "SELECT email,tenant,n,marker FROM conflict_sources ORDER BY tenant",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 3);
    for (row, (email, tenant, n, marker)) in
        [("One", 1, 8, 1), ("Two", 2, 4, 3), ("Three", 3, 6, 6)]
            .into_iter()
            .enumerate()
    {
        assert_eq!(rows.value_at(row, 0), Some(&Value::Str(email.into())));
        assert_eq!(rows.value_at(row, 1), Some(&Value::Int(tenant)));
        assert_eq!(rows.value_at(row, 2), Some(&Value::Int(n)));
        assert_eq!(rows.value_at(row, 3), Some(&Value::Int(marker)));
    }
    assert_eq!(
        engine
            .sql("SELECT last_value FROM renamed_inference_sequence", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(6))
    );
}
