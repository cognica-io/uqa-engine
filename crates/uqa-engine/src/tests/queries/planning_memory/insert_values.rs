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
fn analytical_fixture_single_insert_fits_default_retention(#[case] provider: Provider) {
    use std::fmt::Write;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../benchmarks/analytical/manifest.json"
    )))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("analytical-values.db");
    let engine = provider.open(&path);
    engine
        .sql(fixture["schema_sql"].as_str().unwrap(), &[])
        .unwrap();
    engine.sql("SET work_mem TO '4MB'", &[]).unwrap();
    let memory = engine.query_retention_control().unwrap().memory().clone();
    assert_eq!(memory.limit(), 64 << 20);
    let count = fixture["rows"].as_u64().unwrap();
    let seed = fixture["seed"].as_u64().unwrap();
    let mut sql = String::from("INSERT INTO lineitem (id, return_flag, line_status, quantity, extended_price, discount, ship_day) VALUES ");
    for id in 0..count {
        if id != 0 {
            sql.push_str(", ");
        }
        let sample = id ^ seed;
        write!(
            sql,
            "({id}, '{}', '{}', {}, {}, {}, {})",
            ["A", "N", "R"][sample as usize % 3],
            ["F", "O"][(sample as usize / 3) % 2],
            1 + sample % 50,
            10_000 + sample % 90_000,
            sample % 11,
            sample % 2_500,
        )
        .unwrap();
    }
    engine.sql(&sql, &[]).unwrap();
    let error = engine.sql(&sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"), "{provider:?}: {error}");
    drop(error);
    engine.sql("BEGIN; SAVEPOINT rejected", &[]).unwrap();
    let retained = memory.used();
    let occupied = memory.reserve(memory.available() - (16 << 10)).unwrap();
    let error = engine.sql(&sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    drop(error);
    assert_eq!(memory.used(), retained + occupied.bytes());
    drop(occupied);
    engine.sql("ROLLBACK TO rejected; COMMIT", &[]).unwrap();
    assert_analytical_fixture(&engine, &fixture);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_analytical_fixture(&reopened, &fixture);
        reopened.close().unwrap();
    }
}

fn assert_analytical_fixture(engine: &Engine, fixture: &serde_json::Value) {
    let rows = engine
        .sql(
            "SELECT COUNT(*), MIN(id), MAX(id), CAST(SUM(id) AS BIGINT) FROM lineitem",
            &[],
        )
        .unwrap();
    for (column, value) in [20_000, 0, 19_999, 199_990_000].into_iter().enumerate() {
        assert_eq!(rows.value_at(0, column), Some(&Value::Int(value)));
    }
    drop(rows);
    // SQLite 3.51.0 reference, analytical manifest seed 20260802 with 20,000 rows.
    let rows = engine
        .sql(fixture["queries"]["q1"].as_str().unwrap(), &[])
        .unwrap();
    let expected = [
        ("A", "F", 3285, 82163),
        ("A", "O", 3283, 85338),
        ("N", "F", 3285, 85448),
        ("N", "O", 3282, 82026),
        ("R", "F", 3283, 82055),
        ("R", "O", 3282, 85308),
    ];
    assert_eq!(rows.rows.len(), expected.len());
    for (row, (flag, status, count, quantity)) in expected.into_iter().enumerate() {
        assert_eq!(rows.value_at(row, 0), Some(&Value::Str(flag.into())));
        assert_eq!(rows.value_at(row, 1), Some(&Value::Str(status.into())));
        assert_eq!(rows.value_at(row, 2), Some(&Value::Int(count)));
        assert_eq!(rows.value_at(row, 3), Some(&Value::Int(quantity)));
    }
    drop(rows);
    let rows = engine
        .sql(fixture["queries"]["q6"].as_str().unwrap(), &[])
        .unwrap();
    assert_eq!(rows.value_at(0, 0), Some(&Value::Int(879_578_310)));
    drop(rows);
    let scan = fixture["queries"]["scan"].as_str().unwrap();
    let rows = engine.sql(scan, &[]).unwrap();
    assert_eq!(rows.rows.len(), 15_602);
    let prices: i64 = (0..rows.rows.len())
        .map(|index| match rows.value_at(index, 1) {
            Some(Value::Int(value)) => *value,
            other => panic!("expected extended price: {other:?}"),
        })
        .sum();
    assert_eq!(prices, 345_536_285);
    let cursor = engine.sql_cursor(scan, &[]).unwrap();
    assert_eq!(cursor.summary().row_count, rows.rows.len());
    assert!(!cursor.summary().spilled_to_disk);
    let mut position = 0;
    for batch in cursor {
        let batch = batch.unwrap();
        let columns = batch.columns();
        for (id, price) in columns[0].values.iter().zip(&columns[1].values) {
            assert_eq!(Some(id), rows.value_at(position, 0));
            assert_eq!(Some(price), rows.value_at(position, 1));
            position += 1;
        }
    }
    assert_eq!(position, rows.rows.len());
}

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
