//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Data-modifying WITH items run when `PostgreSQL`'s executor runs them: an item the primary query reads runs before the query draws its rows, the items nothing reads run once the primary query has finished, the last defined first, and every AFTER event of the statement fires once all of them have finished. A row that one command of the statement wrote is one the statement already modified: a later UPDATE or DELETE skips it, and a MERGE or an ON CONFLICT DO UPDATE that reaches it fails. Every expectation was observed on `PostgreSQL` 18.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// The values of the first row `sql` returns.
fn first_row(engine: &Engine, sql: &str) -> Vec<Value> {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result
        .columns
        .iter()
        .map(|column| result.rows[0][column].clone())
        .collect()
}

/// The SQLSTATE and message of the error `sql` fails with.
fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).unwrap_err();
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

/// Tables whose BEFORE and AFTER, ROW and STATEMENT INSERT triggers log their firing to `lg`.
fn traced_tables(engine: &Engine, tables: &[&str]) {
    exec(
        engine,
        "CREATE TABLE lg (seq serial, msg text);
         CREATE FUNCTION lg_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO lg (msg) VALUES (TG_TABLE_NAME || ' ' || TG_WHEN || ' ' || TG_LEVEL); IF TG_WHEN = 'BEFORE' AND TG_LEVEL = 'ROW' THEN RETURN NEW; END IF; RETURN NULL; END $$",
    );
    for table in tables {
        exec(
            engine,
            &format!(
                "CREATE TABLE {table} (a int);
                 CREATE TRIGGER br BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION lg_row();
                 CREATE TRIGGER bs BEFORE INSERT ON {table} FOR EACH STATEMENT EXECUTE FUNCTION lg_row();
                 CREATE TRIGGER ar AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION lg_row();
                 CREATE TRIGGER as_ AFTER INSERT ON {table} FOR EACH STATEMENT EXECUTE FUNCTION lg_row()"
            ),
        );
    }
}

/// The triggers that fired since the last call, in the order they fired.
fn fired(engine: &Engine) -> String {
    let row = first_row(
        engine,
        "SELECT string_agg(msg, ', ' ORDER BY seq) AS fired FROM lg",
    );
    exec(engine, "DELETE FROM lg");
    match &row[0] {
        Value::Str(fired) => fired.clone(),
        other => panic!("unexpected trigger log {other:?}"),
    }
}

#[test]
fn unread_items_run_after_the_primary_query_last_defined_first() {
    let engine = Engine::new();
    traced_tables(&engine, &["t1", "t2", "t3"]);
    exec(&engine, "CREATE SEQUENCE sq");
    exec(
        &engine,
        "WITH i1 AS (INSERT INTO t1 VALUES (nextval('sq')) RETURNING 1), \
         i2 AS (INSERT INTO t2 VALUES (nextval('sq')) RETURNING 1) \
         INSERT INTO t3 VALUES (nextval('sq'))",
    );
    assert_eq!(
        fired(&engine),
        "t3 BEFORE STATEMENT, t3 BEFORE ROW, t2 BEFORE STATEMENT, t2 BEFORE ROW, \
         t1 BEFORE STATEMENT, t1 BEFORE ROW, t3 AFTER ROW, t3 AFTER STATEMENT, \
         t2 AFTER ROW, t2 AFTER STATEMENT, t1 AFTER ROW, t1 AFTER STATEMENT"
    );
    assert_eq!(
        first_row(
            &engine,
            "SELECT (SELECT a FROM t1) AS t1, (SELECT a FROM t2) AS t2, (SELECT a FROM t3) AS t3"
        ),
        vec![Value::Int(3), Value::Int(2), Value::Int(1)]
    );
}

#[test]
fn items_the_primary_query_reads_run_before_it() {
    let engine = Engine::new();
    traced_tables(&engine, &["t1", "t2", "t3"]);
    exec(
        &engine,
        "WITH i1 AS (INSERT INTO t1 VALUES (1) RETURNING a), \
         i2 AS (INSERT INTO t2 VALUES (2) RETURNING a) SELECT * FROM i2",
    );
    assert_eq!(
        fired(&engine),
        "t2 BEFORE STATEMENT, t2 BEFORE ROW, t1 BEFORE STATEMENT, t1 BEFORE ROW, \
         t2 AFTER ROW, t2 AFTER STATEMENT, t1 AFTER ROW, t1 AFTER STATEMENT"
    );
    exec(
        &engine,
        "WITH i1 AS (INSERT INTO t1 VALUES (1), (2) RETURNING a) \
         INSERT INTO t3 SELECT count(*) FROM i1",
    );
    assert_eq!(
        fired(&engine),
        "t3 BEFORE STATEMENT, t1 BEFORE STATEMENT, t1 BEFORE ROW, t1 BEFORE ROW, \
         t3 BEFORE ROW, t1 AFTER ROW, t1 AFTER ROW, t1 AFTER STATEMENT, \
         t3 AFTER ROW, t3 AFTER STATEMENT"
    );
}

#[test]
fn the_statement_fires_its_after_events_once_every_item_has_run() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t1 (a int); CREATE TABLE t3 (a int); CREATE TABLE seen (n bigint);
         CREATE FUNCTION count_t1() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO seen SELECT count(*) FROM t1; RETURN NULL; END $$;
         CREATE TRIGGER count_after AFTER INSERT ON t3 FOR EACH STATEMENT EXECUTE FUNCTION count_t1()",
    );
    exec(
        &engine,
        "WITH i1 AS (INSERT INTO t1 VALUES (7), (8)) INSERT INTO t3 VALUES (1)",
    );
    assert_eq!(
        first_row(&engine, "SELECT n FROM seen"),
        vec![Value::Int(2)]
    );
    exec(
        &engine,
        "DROP TRIGGER count_after ON t3; DELETE FROM t1; DELETE FROM t3;
         CREATE FUNCTION fail_t1() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'item failed'; END $$;
         CREATE TRIGGER fail_after AFTER INSERT ON t1 FOR EACH STATEMENT EXECUTE FUNCTION fail_t1()",
    );
    assert_eq!(
        failure(
            &engine,
            "WITH i1 AS (INSERT INTO t1 VALUES (5)) INSERT INTO t3 VALUES (6)"
        ),
        ("P0001".to_string(), "item failed".to_string())
    );
    assert_eq!(
        first_row(
            &engine,
            "SELECT (SELECT count(*) FROM t1) AS t1, (SELECT count(*) FROM t3) AS t3"
        ),
        vec![Value::Int(0), Value::Int(0)]
    );
}

#[test]
fn an_item_is_checked_against_foreign_keys_when_the_statement_ends() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE cp (id int PRIMARY KEY); CREATE TABLE cc (pid int REFERENCES cp)",
    );
    exec(
        &engine,
        "WITH i AS (INSERT INTO cc VALUES (9) RETURNING 1) INSERT INTO cp VALUES (9)",
    );
    exec(
        &engine,
        "WITH i AS (INSERT INTO cp VALUES (10)) INSERT INTO cc VALUES (10)",
    );
    let error = engine
        .sql(
            "WITH i AS (INSERT INTO cc VALUES (11) RETURNING 1) INSERT INTO cp VALUES (12)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"));
    assert!(
        matches!(&error, uqa_sql::SQLError::Diagnostic { detail: Some(detail), .. }
            if detail == "Key (pid)=(11) is not present in table \"cp\"."),
        "{error:?}"
    );
    assert_eq!(
        first_row(
            &engine,
            "SELECT (SELECT count(*) FROM cp) AS cp, (SELECT count(*) FROM cc) AS cc"
        ),
        vec![Value::Int(2), Value::Int(2)]
    );
}

#[test]
fn an_unread_item_writes_after_the_primary_query() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE u (id int PRIMARY KEY, v int); INSERT INTO u VALUES (1, 0);
         CREATE TABLE nn1 (a int NOT NULL); CREATE TABLE nn2 (a int NOT NULL)",
    );
    assert_eq!(
        failure(
            &engine,
            "WITH d AS (DELETE FROM u WHERE id = 1) INSERT INTO u VALUES (1, 1)"
        ),
        (
            "23505".to_string(),
            "duplicate key value violates unique constraint \"u_pkey\"".to_string()
        )
    );
    assert_eq!(
        failure(
            &engine,
            "WITH i AS (INSERT INTO nn1 VALUES (NULL)) INSERT INTO nn2 VALUES (NULL)"
        ),
        (
            "23502".to_string(),
            "null value in column \"a\" of relation \"nn2\" violates not-null constraint"
                .to_string()
        )
    );
    exec(
        &engine,
        "WITH i AS (INSERT INTO u VALUES (8, 0)) DELETE FROM u WHERE id = 8",
    );
    assert_eq!(
        first_row(&engine, "SELECT count(*) AS n FROM u WHERE id = 8"),
        vec![Value::Int(1)]
    );
}

#[test]
fn a_later_update_or_delete_skips_a_row_the_statement_already_wrote() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE w (id int PRIMARY KEY, v int); INSERT INTO w VALUES (1, 0), (2, 0)",
    );
    let state = |engine: &Engine| {
        engine
            .sql("SELECT id, v FROM w ORDER BY id", &[])
            .unwrap()
            .rows
            .iter()
            .map(|row| (row["id"].clone(), row["v"].clone()))
            .collect::<Vec<_>>()
    };
    let row = |id: i64, v: i64| (Value::Int(id), Value::Int(v));
    exec(
        &engine,
        "WITH x AS (UPDATE w SET v = 1 WHERE id = 1) DELETE FROM w WHERE id = 1",
    );
    assert_eq!(state(&engine), vec![row(2, 0)]);
    exec(&engine, "INSERT INTO w VALUES (1, 0)");
    exec(
        &engine,
        "WITH x AS (DELETE FROM w WHERE id = 1) UPDATE w SET v = 5 WHERE id = 1",
    );
    assert_eq!(state(&engine), vec![row(1, 5), row(2, 0)]);
    exec(
        &engine,
        "WITH x AS (UPDATE w SET v = 6 WHERE id = 1) UPDATE w SET v = 7 WHERE id = 1",
    );
    assert_eq!(state(&engine), vec![row(1, 7), row(2, 0)]);
    let returned = engine
        .sql(
            "WITH x AS (UPDATE w SET v = 8 WHERE id = 1 RETURNING id) \
             UPDATE w SET v = 9 FROM x WHERE w.id = x.id RETURNING w.id",
            &[],
        )
        .unwrap();
    assert!(returned.rows.is_empty());
    assert_eq!(state(&engine), vec![row(1, 8), row(2, 0)]);
    exec(
        &engine,
        "WITH x AS (UPDATE w SET v = 9 WHERE id = 2 RETURNING id) \
         UPDATE w SET v = 10 WHERE id IN (1, 2)",
    );
    assert_eq!(state(&engine), vec![row(1, 10), row(2, 10)]);
}

#[test]
fn merge_and_on_conflict_do_update_reject_a_row_the_statement_already_wrote() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE w (id int PRIMARY KEY, v int); INSERT INTO w VALUES (1, 0);
         CREATE TABLE u (id int PRIMARY KEY, v int); INSERT INTO u VALUES (7, 0)",
    );
    let merge_twice = (
        "21000".to_string(),
        "MERGE command cannot affect row a second time".to_string(),
    );
    let conflict_twice = (
        "21000".to_string(),
        "ON CONFLICT DO UPDATE command cannot affect row a second time".to_string(),
    );
    assert_eq!(
        failure(
            &engine,
            "WITH x AS (UPDATE w SET v = 11 WHERE id = 1 RETURNING id) \
             MERGE INTO w USING x ON w.id = x.id WHEN MATCHED THEN UPDATE SET v = 12"
        ),
        merge_twice
    );
    exec(
        &engine,
        "WITH x AS (UPDATE w SET v = 13 WHERE id = 1) MERGE INTO w USING (VALUES (1)) s(id) \
         ON w.id = s.id WHEN MATCHED THEN UPDATE SET v = 14",
    );
    assert_eq!(
        first_row(&engine, "SELECT v FROM w WHERE id = 1"),
        vec![Value::Int(14)]
    );
    assert_eq!(
        failure(
            &engine,
            "WITH i AS (INSERT INTO u VALUES (5, 0) RETURNING id) \
             INSERT INTO u SELECT id FROM i ON CONFLICT (id) DO UPDATE SET v = 1"
        ),
        conflict_twice
    );
    exec(
        &engine,
        "WITH i AS (INSERT INTO u VALUES (6, 0) RETURNING id) \
         INSERT INTO u SELECT id FROM i ON CONFLICT (id) DO NOTHING",
    );
    assert_eq!(
        failure(
            &engine,
            "WITH i AS (UPDATE u SET v = 3 WHERE id = 7 RETURNING id) \
             INSERT INTO u SELECT id FROM i ON CONFLICT (id) DO UPDATE SET v = 4"
        ),
        conflict_twice
    );
    let rows = engine
        .sql("SELECT id, v FROM u ORDER BY id", &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| (row["id"].clone(), row["v"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            (Value::Int(6), Value::Int(0)),
            (Value::Int(7), Value::Int(0))
        ]
    );
}
