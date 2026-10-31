//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::mutation::{dispatch, insert::table::InsertPlanning};
use uqa_sql::plan::{CommandPlan, RelationalPlan};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_set_returning_values_admit_private_commands_before_copying(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("set-returning-values-owner.db"));
    engine
        .sql(
            "CREATE TABLE items(id integer PRIMARY KEY,flag integer DEFAULT 7)",
            &[],
        )
        .unwrap();
    let sql = "INSERT INTO public.items VALUES(generate_series(1,2),DEFAULT) RETURNING id,flag";
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    let CommandPlan::Insert(mut plan) = *command else {
        panic!("INSERT")
    };
    let UnifiedPlan::Query(mut unused) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("block")
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    plan.subqueries.push(*unused);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut outcome = None;
    let measured = allocation_counter::measure(|| {
        outcome = Some(
            dispatch::run_insert(
                &engine.mutation_statement_context(),
                InsertPlanning {
                    inference: engine.inference_context(),
                    returning: engine.returning_analysis_context(),
                    prune_source_outputs:
                        uqa_planner::mutation_outputs::prune_unused_query_outputs_owned,
                },
                &plan,
                &[],
                None,
            )
            .map(drop),
        );
    });
    let error = outcome.unwrap().unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    assert!(
        measured.bytes_total < 256 << 10,
        "{provider:?}: {measured:?}"
    );
    drop(error);
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    let rows = engine.sql(sql, &[]).unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(rows.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(rows.value_at(0, 1), Some(&Value::Int(7)));
    drop(rows);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn set_returning_values_preserve_postgresql_defaults_rules_and_reopen(#[case] provider: Provider) {
    // PostgreSQL 18.4 reference includes target-list expansion, partial array writes and rule substitution diagnostics.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("set-returning-values-reference.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(a integer DEFAULT 9,b integer,c integer[]); CREATE VIEW v AS SELECT b,c,a FROM items; CREATE TABLE observed(id integer,flag integer DEFAULT 7); CREATE TABLE audit(flag integer); CREATE RULE logflag AS ON INSERT TO observed DO ALSO INSERT INTO audit VALUES(NEW.flag)",&[]).unwrap();
    for (sql, first, array) in [
        (
            "INSERT INTO items VALUES(DEFAULT,generate_series(1,2),ARRAY[5]) RETURNING a,b,c[1]",
            1,
            5,
        ),
        (
            "INSERT INTO items(b,c[1]) VALUES(generate_series(3,4),7) RETURNING a,b,c[1]",
            3,
            7,
        ),
        (
            "INSERT INTO v VALUES(generate_series(5,6),ARRAY[11],DEFAULT) RETURNING a,b,c[1]",
            5,
            11,
        ),
    ] {
        let rows = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{provider:?}: {sql}: {error}"));
        assert_eq!(rows.rows.len(), 2);
        for row in 0..2 {
            assert_eq!(rows.value_at(row, 0), Some(&Value::Int(9)));
            assert_eq!(rows.value_at(row, 1), Some(&Value::Int(first + row as i64)));
            assert_eq!(rows.value_at(row, 2), Some(&Value::Int(array)));
        }
    }
    let rows = engine
        .sql(
            "INSERT INTO observed VALUES(generate_series(1,2),5) RETURNING id,flag",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(rows.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(rows.value_at(0, 1), Some(&Value::Int(5)));
    drop(rows);
    engine.sql("CREATE RULE logid AS ON INSERT TO observed DO ALSO INSERT INTO audit VALUES(NEW.id); BEGIN; SAVEPOINT rejected",&[]).unwrap();
    let error = engine
        .sql("INSERT INTO observed VALUES(generate_series(3,4),6)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"), "{provider:?}: {error}");
    engine.sql("ROLLBACK TO rejected; COMMIT", &[]).unwrap();
    assert_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_rows(&reopened);
        reopened.close().unwrap();
    }
}

fn assert_rows(engine: &Engine) {
    let rows = engine
        .sql("SELECT a,b,c[1] FROM items ORDER BY b", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 6);
    for row in 0..6 {
        assert_eq!(rows.value_at(row, 0), Some(&Value::Int(9)));
        assert_eq!(rows.value_at(row, 1), Some(&Value::Int(row as i64 + 1)));
        assert_eq!(
            rows.value_at(row, 2),
            Some(&Value::Int([5, 5, 7, 7, 11, 11][row]))
        );
    }
    let rows = engine
        .sql("SELECT id,flag FROM observed ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    for row in 0..2 {
        assert_eq!(rows.value_at(row, 0), Some(&Value::Int(row as i64 + 1)));
        assert_eq!(rows.value_at(row, 1), Some(&Value::Int(5)));
    }
    let audit = engine.sql("SELECT flag FROM audit", &[]).unwrap();
    assert_eq!(audit.rows.len(), 1);
    assert_eq!(audit.value_at(0, 0), Some(&Value::Int(5)));
}
