//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn shared_cte_producers_close_at_query_completion(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("cte.db"));
    for (query, expected_rows, expected_reads) in [
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT 1", 1, 1),
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT 0", 0, 0),
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT (SELECT n FROM c LIMIT 1) AS a, (SELECT n FROM c LIMIT 1 OFFSET 1) AS b", 1, 2),
        ("WITH outer_cte AS MATERIALIZED (WITH inner_cte AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM inner_cte LIMIT 2) SELECT * FROM outer_cte LIMIT 1", 1, 1),
        ("SELECT * FROM (WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT 1) r LIMIT 1", 1, 1),
        ("SELECT * FROM (VALUES (2),(3)) v(n) CROSS JOIN LATERAL (WITH c AS MATERIALIZED (SELECT * FROM host_rows(v.n)) SELECT * FROM c LIMIT 1) r", 2, 2),
        ("SELECT * FROM (WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT 1) r UNION ALL SELECT -1", 2, 1),
    ] {
        for close_error in [false, true] {
            let counts = register(&engine, Behavior { close_error, ..Behavior::default() });
            let result = engine.sql(query, &[]);
            if close_error {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("58030"), "{query}: {error:?}");
                assert_eq!(error.hint(), Some("host close failed hint"));
            } else {
                let result = result.unwrap_or_else(|error| panic!("{query}: {error:?}"));
                assert_eq!(result.rows.len(), expected_rows, "{query}");
                assert_eq!(counts.read.load(Ordering::SeqCst), expected_reads, "{query}");
            }
            assert_closed(&counts, query);
            assert_eq!(engine.transaction_depth(), 0);
            engine.sql("SELECT 1", &[]).unwrap();
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn cte_cleanup_keeps_execution_and_construction_failures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("cte.db"));
    for (query, state, read_error) in [
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT 1/n FROM c LIMIT 1", "22012", None),
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT -1", "2201W", None),
        ("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c", "58000", Some(1)),
        ("WITH a AS MATERIALIZED (SELECT * FROM host_rows(100)), b AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT 1/a.n FROM a CROSS JOIN b LIMIT 1", "22012", None),
    ] {
        let counts = register(&engine, Behavior { close_error: true, read_error, ..Behavior::default() });
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{query}: {error:?}");
        assert!(error.detail().unwrap_or_default().contains("host close failed hint"), "{query}: {error:?}");
        assert_closed(&counts, query);
        assert_eq!(engine.transaction_depth(), 0);
        engine.sql("SELECT 1", &[]).unwrap();
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn command_cte_cleanup_precedes_statement_success_and_preserves_failure(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("cte.db"));
    engine
        .sql(
            "CREATE TABLE target(n INTEGER); CREATE VIEW target_view AS SELECT * FROM target",
            &[],
        )
        .unwrap();
    engine.sql("CREATE VIEW trigger_view AS SELECT * FROM target; CREATE FUNCTION apply_target() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF TG_OP = 'INSERT' THEN INSERT INTO target VALUES (NEW.n); RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE target SET n = NEW.n WHERE n = OLD.n; RETURN NEW; ELSE DELETE FROM target WHERE n = OLD.n; RETURN OLD; END IF; END $$; CREATE TRIGGER apply_target INSTEAD OF INSERT OR UPDATE OR DELETE ON trigger_view FOR EACH ROW EXECUTE FUNCTION apply_target()", &[]).unwrap();
    for target in ["target", "target_view", "trigger_view"] {
        for command in [
            format!("INSERT INTO {target} SELECT n FROM c LIMIT 1"),
            format!("UPDATE {target} SET n = (SELECT n FROM c LIMIT 1)"),
            format!("DELETE FROM {target} WHERE n > (SELECT n FROM c LIMIT 1)"),
            format!("MERGE INTO {target} t USING (SELECT n FROM c LIMIT 1) s ON t.n = s.n WHEN NOT MATCHED THEN INSERT VALUES (s.n)"),
        ] {
            for read_error in [None, Some(0)] {
            for close_error in [false, true] {
                engine.sql("DELETE FROM target; INSERT INTO target VALUES (10), (11)", &[]).unwrap();
                let query = format!("WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) {command}");
                let counts = register(&engine, Behavior { close_error, read_error, ..Behavior::default() });
                let result = engine.sql(&query, &[]);
                if read_error.is_some() || close_error {
                    let error = match result { Err(error) => error, Ok(result) => panic!("{query}: expected failure with read_error={read_error:?}, close_error={close_error}; got {result:?}") };
                    let state = if read_error.is_some() { "58000" } else { "58030" };
                    assert_eq!(error.sqlstate(), Some(state), "{query}: {error:?}");
                    if close_error && read_error.is_some() {
                        assert!(error.detail().unwrap_or_default().contains("host close failed hint"), "{query}: {error:?}");
                    }
                    assert_eq!(engine.sql("SELECT sum(n) AS n FROM target", &[]).unwrap().rows[0]["n"], Value::Int(21), "{query}");
                } else {
                    result.unwrap_or_else(|error| panic!("{query}: {error:?}"));
                }
                assert_closed(&counts, &query);
                assert_eq!(engine.transaction_depth(), 0);
            }
        }
    }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn cte_cleanup_reaches_prepared_cursor_and_cancellation_owners(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("cte.db"));
    engine
        .sql(
            "CREATE TABLE retained(n INTEGER); INSERT INTO retained VALUES (1)",
            &[],
        )
        .unwrap();
    let query = "WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c LIMIT 1";
    let counts = register(
        &engine,
        Behavior {
            close_error: true,
            ..Behavior::default()
        },
    );
    engine
        .sql(&format!("PREPARE retained_cte AS {query}"), &[])
        .unwrap();
    engine
        .sql(
            "BEGIN; SAVEPOINT before_cte; INSERT INTO retained VALUES (2)",
            &[],
        )
        .unwrap();
    let error = engine.sql("EXECUTE retained_cte", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("58030"));
    assert_closed(&counts, "prepared CTE");
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    engine.sql("ROLLBACK TO before_cte; COMMIT", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM retained", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );

    let counts = register(
        &engine,
        Behavior {
            close_error: true,
            ..Behavior::default()
        },
    );
    let Err(error) = engine.sql_cursor(query, &[]) else {
        panic!("cursor construction lost CTE cleanup failure");
    };
    assert_eq!(error.sqlstate(), Some("58030"));
    assert_closed(&counts, "cursor CTE");

    let counts = register(
        &engine,
        Behavior {
            close_error: true,
            cancel: true,
            ..Behavior::default()
        },
    );
    let error = engine
        .sql(
            "WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)) SELECT * FROM c",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(error
        .detail()
        .unwrap_or_default()
        .contains("host close failed hint"));
    assert_closed(&counts, "cancelled CTE");
    assert!(engine.is_cancelled());
    engine.reset_cancellation();
    engine.sql("SELECT 1", &[]).unwrap();
    assert_eq!(engine.transaction_depth(), 0);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn view_merge_cte_construction_failure_closes_preceding_producers(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("merge-scope.db"));
    engine.sql("CREATE TABLE target(n INTEGER); CREATE VIEW trigger_view AS SELECT * FROM target; CREATE FUNCTION insert_target() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO target VALUES (NEW.n); RETURN NEW; END $$; CREATE TRIGGER insert_target INSTEAD OF INSERT ON trigger_view FOR EACH ROW EXECUTE FUNCTION insert_target()", &[]).unwrap();
    let counts = register(
        &engine,
        Behavior {
            close_error: true,
            ..Behavior::default()
        },
    );
    let query = "WITH c AS MATERIALIZED (SELECT * FROM host_rows(100)), bad AS MATERIALIZED (SELECT d.n FROM c CROSS JOIN host_rows(NULL::integer) d) MERGE INTO trigger_view t USING bad s ON t.n = s.n WHEN NOT MATCHED THEN INSERT VALUES (s.n)";
    let error = engine.sql(query, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"), "{error:?}");
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("host close failed hint"),
        "{error:?}"
    );
    assert_closed(&counts, query);
    assert_eq!(counts.read.load(Ordering::SeqCst), 0);
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM target", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );
}
