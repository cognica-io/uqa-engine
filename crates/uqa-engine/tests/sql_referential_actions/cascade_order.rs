//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cascading referential actions run when their internal triggers fire, as `PostgreSQL`'s `RI_FKey_cascade_del` and its siblings do: once the statement has written its rows, among the referenced row's AFTER ROW triggers in the order of the triggers' names, with the cascaded rows' AFTER events appended to the end of the statement's queue. Every expectation was observed on `PostgreSQL` 18.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// A log of trigger firings: the table, the trigger, its timing, event and level, and the row's `id` for a row trigger.
fn trigger_log(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE clog (seq serial, msg text);
         CREATE FUNCTION clog_row() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE detail text := '';
         BEGIN
           IF TG_LEVEL = 'ROW' THEN
             IF TG_OP = 'DELETE' THEN detail := ' ' || OLD.id; ELSE detail := ' ' || NEW.id; END IF;
           END IF;
           INSERT INTO clog (msg) VALUES (TG_TABLE_NAME || ' ' || TG_NAME || ' ' || TG_WHEN || ' ' || TG_OP || ' ' || TG_LEVEL || detail);
           IF TG_WHEN = 'BEFORE' AND TG_LEVEL = 'ROW' THEN
             IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
             RETURN NEW;
           END IF;
           RETURN NULL;
         END $$",
    );
}

/// The triggers that fired since the last call, in the order they fired.
fn fired(engine: &Engine) -> String {
    let result = engine
        .sql(
            "SELECT string_agg(msg, ' | ' ORDER BY seq) AS fired FROM clog",
            &[],
        )
        .unwrap();
    exec(engine, "DELETE FROM clog");
    match &result.rows[0]["fired"] {
        Value::Str(fired) => fired.clone(),
        other => panic!("unexpected trigger log {other:?}"),
    }
}

#[test]
fn a_cascade_runs_when_its_internal_trigger_fires() {
    let engine = Engine::new();
    trigger_log(&engine);
    exec(
        &engine,
        "CREATE TABLE cpar (id int PRIMARY KEY);
         CREATE TABLE cchi (id int PRIMARY KEY, pid int REFERENCES cpar ON DELETE CASCADE);
         CREATE TRIGGER \"A_log\" AFTER DELETE ON cpar FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER z_log AFTER DELETE ON cpar FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s_log AFTER DELETE ON cpar FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER b_chi BEFORE DELETE ON cchi FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER a_chi AFTER DELETE ON cchi FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s_chi AFTER DELETE ON cchi FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER bs_chi BEFORE DELETE ON cchi FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         INSERT INTO cpar VALUES (1), (2);
         INSERT INTO cchi VALUES (10, 1), (11, 1), (20, 2)",
    );
    exec(&engine, "DELETE FROM cpar");
    assert_eq!(
        fired(&engine),
        "cpar A_log AFTER DELETE ROW 1 | cchi bs_chi BEFORE DELETE STATEMENT | \
         cchi b_chi BEFORE DELETE ROW 10 | cchi b_chi BEFORE DELETE ROW 11 | \
         cpar z_log AFTER DELETE ROW 1 | cpar A_log AFTER DELETE ROW 2 | \
         cchi b_chi BEFORE DELETE ROW 20 | cpar z_log AFTER DELETE ROW 2 | \
         cpar s_log AFTER DELETE STATEMENT | cchi a_chi AFTER DELETE ROW 10 | \
         cchi a_chi AFTER DELETE ROW 11 | cchi a_chi AFTER DELETE ROW 20 | \
         cchi s_chi AFTER DELETE STATEMENT"
    );
}

#[test]
fn each_cascade_level_appends_its_events_to_the_statement_queue() {
    let engine = Engine::new();
    trigger_log(&engine);
    exec(
        &engine,
        "CREATE TABLE gp (id int PRIMARY KEY);
         CREATE TABLE pa (id int PRIMARY KEY, gid int REFERENCES gp ON DELETE CASCADE);
         CREATE TABLE ch (id int PRIMARY KEY, pid int REFERENCES pa ON DELETE CASCADE);
         CREATE TRIGGER r AFTER DELETE ON gp FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s AFTER DELETE ON gp FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER r AFTER DELETE ON pa FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s AFTER DELETE ON pa FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER b BEFORE DELETE ON pa FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER r AFTER DELETE ON ch FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s AFTER DELETE ON ch FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER b BEFORE DELETE ON ch FOR EACH ROW EXECUTE FUNCTION clog_row();
         INSERT INTO gp VALUES (1), (2);
         INSERT INTO pa VALUES (10, 1), (20, 2);
         INSERT INTO ch VALUES (100, 10), (101, 10), (200, 20)",
    );
    exec(&engine, "DELETE FROM gp");
    assert_eq!(
        fired(&engine),
        "pa b BEFORE DELETE ROW 10 | gp r AFTER DELETE ROW 1 | pa b BEFORE DELETE ROW 20 | \
         gp r AFTER DELETE ROW 2 | gp s AFTER DELETE STATEMENT | ch b BEFORE DELETE ROW 100 | \
         ch b BEFORE DELETE ROW 101 | pa r AFTER DELETE ROW 10 | ch b BEFORE DELETE ROW 200 | \
         pa r AFTER DELETE ROW 20 | pa s AFTER DELETE STATEMENT | ch r AFTER DELETE ROW 100 | \
         ch r AFTER DELETE ROW 101 | ch r AFTER DELETE ROW 200 | ch s AFTER DELETE STATEMENT"
    );
}

#[test]
fn update_cascades_and_set_null_run_when_their_triggers_fire() {
    let engine = Engine::new();
    trigger_log(&engine);
    exec(
        &engine,
        "CREATE TABLE up (id int PRIMARY KEY);
         CREATE TABLE uc (id int PRIMARY KEY, pid int REFERENCES up ON UPDATE CASCADE ON DELETE SET NULL);
         CREATE TRIGGER r AFTER UPDATE ON uc FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s AFTER UPDATE ON uc FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER b BEFORE UPDATE ON uc FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER r_update AFTER UPDATE ON up FOR EACH ROW EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s_update AFTER UPDATE ON up FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER r_delete AFTER DELETE ON up FOR EACH ROW EXECUTE FUNCTION clog_row();
         INSERT INTO up VALUES (1), (2);
         INSERT INTO uc VALUES (10, 1), (20, 2)",
    );
    exec(&engine, "UPDATE up SET id = id + 10");
    assert_eq!(
        fired(&engine),
        "uc b BEFORE UPDATE ROW 10 | up r_update AFTER UPDATE ROW 11 | \
         uc b BEFORE UPDATE ROW 20 | up r_update AFTER UPDATE ROW 12 | \
         up s_update AFTER UPDATE STATEMENT | uc r AFTER UPDATE ROW 10 | \
         uc r AFTER UPDATE ROW 20 | uc s AFTER UPDATE STATEMENT"
    );
    exec(&engine, "DELETE FROM up WHERE id = 11");
    assert_eq!(
        fired(&engine),
        "uc b BEFORE UPDATE ROW 10 | up r_delete AFTER DELETE ROW 11 | \
         uc r AFTER UPDATE ROW 10 | uc s AFTER UPDATE STATEMENT"
    );
    let rows = engine
        .sql("SELECT id, pid FROM uc ORDER BY id", &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| (row["id"].clone(), row["pid"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            (Value::Int(10), Value::Null),
            (Value::Int(20), Value::Int(12))
        ]
    );
}

#[test]
fn a_later_row_violation_is_reported_before_a_cascade_violation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE sp (id int PRIMARY KEY);
         CREATE TABLE sc (id int PRIMARY KEY, pid int NOT NULL REFERENCES sp ON UPDATE SET NULL);
         INSERT INTO sp VALUES (1), (2), (3);
         INSERT INTO sc VALUES (10, 1)",
    );
    let error = engine
        .sql(
            "UPDATE sp SET id = CASE id WHEN 1 THEN 5 WHEN 2 THEN 3 ELSE 4 END",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"), "{error}");
    assert_eq!(
        error.to_string(),
        "duplicate key value violates unique constraint \"sp_pkey\""
    );
}

#[test]
fn a_cascade_into_the_statement_table_joins_its_transition_table() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tree (id int PRIMARY KEY, parent int REFERENCES tree ON DELETE CASCADE);
         CREATE TABLE tlog (n bigint, ids text);
         CREATE FUNCTION tree_count() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO tlog SELECT count(*), string_agg(id::text, ',' ORDER BY id) FROM old_rows; RETURN NULL; END $$;
         CREATE TRIGGER tree_stmt AFTER DELETE ON tree REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION tree_count();
         INSERT INTO tree VALUES (1, NULL), (2, 1), (3, 1), (4, 2)",
    );
    exec(&engine, "DELETE FROM tree WHERE id = 1");
    let rows = engine
        .sql("SELECT n, ids FROM tlog", &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| (row["n"].clone(), row["ids"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(rows, vec![(Value::Int(4), Value::Str("1,2,3,4".into()))]);
}

#[test]
fn a_cascade_that_writes_no_row_still_fires_its_statement_triggers() {
    let engine = Engine::new();
    trigger_log(&engine);
    exec(
        &engine,
        "CREATE TABLE np (id int PRIMARY KEY);
         CREATE TABLE nc (id int PRIMARY KEY, pid int REFERENCES np ON DELETE CASCADE);
         CREATE TRIGGER bs BEFORE DELETE ON nc FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         CREATE TRIGGER s AFTER DELETE ON nc FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         INSERT INTO np VALUES (1), (2)",
    );
    exec(&engine, "DELETE FROM np WHERE id = 1");
    assert_eq!(
        fired(&engine),
        "nc bs BEFORE DELETE STATEMENT | nc s AFTER DELETE STATEMENT"
    );
}

#[test]
fn cascaded_rows_join_the_transition_tables_until_a_trigger_reads_them() {
    let engine = Engine::new();
    trigger_log(&engine);
    exec(
        &engine,
        "CREATE TABLE xp (id int PRIMARY KEY);
         CREATE TABLE xc (id int PRIMARY KEY, pid int REFERENCES xp ON DELETE CASCADE);
         CREATE TABLE xlog (seq serial, msg text);
         CREATE FUNCTION xc_count() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO xlog (msg) SELECT TG_NAME || ' ' || TG_LEVEL || ' ' || count(*) FROM old_rows; RETURN NULL; END $$;
         CREATE TRIGGER xr AFTER DELETE ON xc REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION xc_count();
         CREATE TRIGGER xs AFTER DELETE ON xc REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION xc_count();
         CREATE TRIGGER bs BEFORE DELETE ON xc FOR EACH STATEMENT EXECUTE FUNCTION clog_row();
         INSERT INTO xp VALUES (1), (2);
         INSERT INTO xc VALUES (10, 1), (11, 1), (20, 2)",
    );
    exec(&engine, "DELETE FROM xp");
    let counts = engine
        .sql(
            "SELECT string_agg(msg, ' | ' ORDER BY seq) AS fired FROM xlog",
            &[],
        )
        .unwrap();
    assert_eq!(
        counts.rows[0]["fired"],
        Value::Str("xr ROW 3 | xr ROW 3 | xr ROW 3 | xs STATEMENT 3".into())
    );
    assert_eq!(fired(&engine), "xc bs BEFORE DELETE STATEMENT");
}

#[test]
fn the_last_action_on_a_table_selects_its_after_statement_triggers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE mp (a int UNIQUE, b int UNIQUE);
         CREATE TABLE mc (id int PRIMARY KEY, a int REFERENCES mp(a) ON UPDATE CASCADE, b int REFERENCES mp(b) ON UPDATE CASCADE);
         CREATE TABLE mlg (seq serial, msg text);
         CREATE FUNCTION mlg_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO mlg (msg) VALUES (TG_NAME || ':' || TG_WHEN); RETURN NULL; END $$;
         CREATE TRIGGER before_a BEFORE UPDATE OF a ON mc FOR EACH STATEMENT EXECUTE FUNCTION mlg_probe();
         CREATE TRIGGER before_b BEFORE UPDATE OF b ON mc FOR EACH STATEMENT EXECUTE FUNCTION mlg_probe();
         CREATE TRIGGER after_a AFTER UPDATE OF a ON mc FOR EACH STATEMENT EXECUTE FUNCTION mlg_probe();
         CREATE TRIGGER after_b AFTER UPDATE OF b ON mc FOR EACH STATEMENT EXECUTE FUNCTION mlg_probe();
         INSERT INTO mp VALUES (1, 10), (2, 20);
         INSERT INTO mc VALUES (1, 1, 10), (2, 2, 20)",
    );
    exec(&engine, "UPDATE mp SET a = a + 100, b = b + 1000");
    let fired = engine
        .sql("SELECT string_agg(msg, ' | ' ORDER BY seq) AS fired FROM mlg", &[])
        .unwrap();
    assert_eq!(
        fired.rows[0]["fired"],
        Value::Str("before_a:BEFORE | after_b:AFTER".into())
    );
}
