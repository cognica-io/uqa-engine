//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement that reaches a row which a statement started by its own triggers or functions modified fails, as `PostgreSQL` finds the row modified under a later command id (`TM_SelfModified`) and reports `27000`. Every expectation was observed on `PostgreSQL` 18.

use super::exec;
use uqa_core::Value;
use uqa_engine::Engine;

const HINT: &str =
    "Consider using an AFTER trigger instead of a BEFORE trigger to propagate changes to other rows.";

/// The SQLSTATE, message and hint of the error `sql` fails with.
fn failure(engine: &Engine, sql: &str) -> (String, String, Option<String>) {
    let error = engine
        .sql(sql, &[])
        .err()
        .unwrap_or_else(|| panic!("{sql} succeeded"));
    let hint = match &error {
        uqa_sql::SQLError::Diagnostic { hint, .. } => hint.clone(),
        _ => None,
    };
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
        hint,
    )
}

fn modified(operation: &str) -> (String, String, Option<String>) {
    (
        "27000".into(),
        format!(
            "tuple to be {operation} was already modified by an operation triggered by the current command"
        ),
        Some(HINT.into()),
    )
}

fn values(engine: &Engine) -> Vec<(Value, Value)> {
    exec(engine, "SELECT id, v FROM tm ORDER BY id")
        .rows
        .into_iter()
        .map(|row| (row["id"].clone(), row["v"].clone()))
        .collect()
}

fn unchanged() -> Vec<(Value, Value)> {
    vec![
        (Value::Int(1), Value::Int(0)),
        (Value::Int(2), Value::Int(0)),
    ]
}

#[test]
fn a_row_its_before_row_trigger_modified_stops_the_statement() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tm (id int PRIMARY KEY, v int); INSERT INTO tm VALUES (1, 0), (2, 0);
         CREATE FUNCTION tm_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.id = 1 THEN UPDATE tm SET v = 99 WHERE id = 2; END IF; IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $$;
         CREATE TRIGGER touch BEFORE UPDATE OR DELETE ON tm FOR EACH ROW EXECUTE FUNCTION tm_touch()",
    );
    for sql in [
        "UPDATE tm SET v = v + 1",
        "DELETE FROM tm",
        "MERGE INTO tm USING (VALUES (1), (2)) s(id) ON tm.id = s.id WHEN MATCHED THEN UPDATE SET v = 5",
    ] {
        assert_eq!(failure(&engine, sql), modified("updated"), "{sql}");
        assert_eq!(values(&engine), unchanged(), "{sql}");
    }
}

#[test]
fn a_row_its_statement_trigger_or_functions_modified_stops_the_statement() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tm (id int PRIMARY KEY, v int); INSERT INTO tm VALUES (1, 0), (2, 0);
         CREATE FUNCTION tm_stmt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF pg_trigger_depth() = 1 THEN UPDATE tm SET v = 50 WHERE id = 2; END IF; RETURN NULL; END $$;
         CREATE TRIGGER stmt BEFORE UPDATE ON tm FOR EACH STATEMENT EXECUTE FUNCTION tm_stmt()",
    );
    assert_eq!(
        failure(&engine, "UPDATE tm SET v = v + 1 WHERE id = 2"),
        modified("updated")
    );
    assert_eq!(values(&engine), unchanged());
    exec(
        &engine,
        "DROP TRIGGER stmt ON tm;
         CREATE FUNCTION tm_func(i int) RETURNS int LANGUAGE plpgsql VOLATILE AS $$ BEGIN IF i = 1 THEN UPDATE tm SET v = 77 WHERE id = 2; END IF; RETURN 1; END $$",
    );
    assert_eq!(
        failure(&engine, "UPDATE tm SET v = v + tm_func(id)"),
        modified("updated")
    );
    assert_eq!(
        failure(&engine, "DELETE FROM tm WHERE tm_func(id) = 1"),
        modified("deleted")
    );
    assert_eq!(values(&engine), unchanged());
    exec(
        &engine,
        "CREATE TABLE tside (id int);
         CREATE FUNCTION side_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE tm SET v = 88 WHERE id = 2; RETURN NEW; END $$;
         CREATE TRIGGER side BEFORE INSERT ON tside FOR EACH ROW EXECUTE FUNCTION side_touch();
         CREATE FUNCTION tm_ins(i int) RETURNS int LANGUAGE plpgsql VOLATILE AS $$ BEGIN IF i = 1 THEN INSERT INTO tside VALUES (1); END IF; RETURN 1; END $$",
    );
    assert_eq!(
        failure(&engine, "UPDATE tm SET v = tm_ins(id)"),
        modified("updated")
    );
    assert_eq!(values(&engine), unchanged());
}

#[test]
fn pg_trigger_depth_counts_the_trigger_functions_the_code_runs_in() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE td1 (id int); CREATE TABLE td2 (id int); CREATE TABLE tdl (seq serial, msg text);
         CREATE FUNCTION td_log() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO tdl (msg) VALUES (TG_TABLE_NAME || ' ' || pg_trigger_depth()); IF TG_TABLE_NAME = 'td1' THEN INSERT INTO td2 VALUES (NEW.id); END IF; RETURN NULL; END $$;
         CREATE TRIGGER l AFTER INSERT ON td1 FOR EACH ROW EXECUTE FUNCTION td_log();
         CREATE TRIGGER l AFTER INSERT ON td2 FOR EACH ROW EXECUTE FUNCTION td_log();
         INSERT INTO td1 VALUES (1)",
    );
    let row = exec(
        &engine,
        "SELECT pg_trigger_depth() AS depth, (SELECT string_agg(msg, ', ' ORDER BY seq) FROM tdl) AS fired",
    );
    assert_eq!(row.rows[0]["depth"], Value::Int(0));
    assert_eq!(row.rows[0]["fired"], Value::Str("td1 1, td2 2".into()));
    let catalog = exec(
        &engine,
        "SELECT proname::text AS name, provolatile::text AS volatility FROM pg_proc WHERE oid = 3163",
    );
    assert_eq!(
        catalog.rows[0]["name"],
        Value::Str("pg_trigger_depth".into())
    );
    assert_eq!(catalog.rows[0]["volatility"], Value::Str("s".into()));
}
