//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the extended SQL surface:
//! `INSERT ... SELECT`, `CREATE VIEW`, `CREATE SCHEMA`, `EXPLAIN`,
//! `ANALYZE`, `TRUNCATE`, transaction control statements.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::ManagedConnection;

#[test]
fn insert_from_select_copies_rows() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE src (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE dst (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO src (body) VALUES ('hello')", &[])
        .unwrap();
    eng.sql("INSERT INTO src (body) VALUES ('world')", &[])
        .unwrap();
    eng.sql("INSERT INTO dst (body) SELECT body FROM src", &[])
        .unwrap();
    let res = eng.sql("SELECT body FROM dst ORDER BY id", &[]).unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0]["body"], Value::Str("hello".into()));
    assert_eq!(res.rows[1]["body"], Value::Str("world".into()));
}

#[test]
fn create_view_and_drop_round_trip() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE VIEW v AS SELECT id, body FROM notes", &[])
        .unwrap();
    assert!(eng.view("v").unwrap().is_some());
}

#[test]
fn create_view_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("views.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql(
            "CREATE TABLE app.notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
        eng.sql("INSERT INTO notes (body) VALUES ('hello')", &[])
            .unwrap();
        eng.sql("CREATE VIEW app.note_bodies AS SELECT body FROM notes", &[])
            .unwrap();
    }
    let eng = Engine::open(&db).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    let rows = eng.sql("SELECT body FROM note_bodies", &[]).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["body"], Value::Str("hello".into()));
}

#[test]
fn create_schema_records_name() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE SCHEMA IF NOT EXISTS app", &[]).unwrap();
    assert!(eng.drop_schema("app").unwrap());
}

#[test]
fn truncate_wipes_rows_keeping_schema() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('a')", &[]).unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('b')", &[]).unwrap();
    eng.sql("TRUNCATE TABLE t", &[]).unwrap();
    let res = eng.sql("SELECT body FROM t", &[]).unwrap();
    assert!(res.rows.is_empty());
    // Schema still intact: we can still INSERT.
    eng.sql("INSERT INTO t (body) VALUES ('c')", &[]).unwrap();
}

#[test]
fn truncate_honors_foreign_key_boundaries_and_cascade() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
        &[],
    )
    .unwrap();
    assert_eq!(
        eng.foreign_keys("child").unwrap()[0].ref_table,
        "public.parent"
    );
    eng.sql("INSERT INTO parent (id) VALUES (1)", &[]).unwrap();
    eng.sql("INSERT INTO child (id, parent_id) VALUES (10, 1)", &[])
        .unwrap();

    let error = eng.sql("TRUNCATE parent", &[]).unwrap_err();
    assert!(error.to_string().contains("references"), "{error}");
    eng.sql("TRUNCATE parent, public.parent, child", &[])
        .unwrap();
    assert!(eng
        .sql("SELECT id FROM parent", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(eng
        .sql("SELECT id FROM child", &[])
        .unwrap()
        .rows
        .is_empty());

    eng.sql("INSERT INTO parent (id) VALUES (2)", &[]).unwrap();
    eng.sql("INSERT INTO child (id, parent_id) VALUES (20, 2)", &[])
        .unwrap();
    eng.sql("TRUNCATE parent CASCADE", &[]).unwrap();
    assert!(eng
        .sql("SELECT id FROM parent", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(eng
        .sql("SELECT id FROM child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn truncate_uses_the_foreign_keys_creation_schema() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE TABLE public.parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("CREATE TABLE app.parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.child (\
             id INTEGER PRIMARY KEY, \
             parent_id INTEGER, \
             FOREIGN KEY (parent_id) REFERENCES parent(id)\
         )",
        &[],
    )
    .unwrap();
    assert_eq!(
        eng.foreign_keys("app.child").unwrap()[0].ref_table,
        "app.parent"
    );
    eng.sql("INSERT INTO public.parent (id) VALUES (1)", &[])
        .unwrap();
    eng.sql("INSERT INTO app.parent (id) VALUES (2)", &[])
        .unwrap();
    eng.sql("INSERT INTO app.child (id, parent_id) VALUES (20, 2)", &[])
        .unwrap();

    eng.sql("SET search_path TO public, app", &[]).unwrap();
    eng.sql("TRUNCATE public.parent", &[]).unwrap();
    assert_eq!(
        eng.sql("SELECT COUNT(*) AS n FROM app.parent", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
    let delete_error = eng
        .sql("DELETE FROM app.parent WHERE id = 2", &[])
        .unwrap_err();
    assert!(
        delete_error.to_string().contains("referenced"),
        "{delete_error}"
    );
    let truncate_error = eng.sql("TRUNCATE app.parent", &[]).unwrap_err();
    assert!(
        truncate_error.to_string().contains("app.child"),
        "{truncate_error}"
    );
    eng.sql("TRUNCATE app.parent, app.child", &[]).unwrap();
}

#[test]
fn canonical_foreign_keys_and_truncate_cascade_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("truncate-fk.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql("CREATE TABLE parent (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        eng.sql(
            "CREATE TABLE child (\
                 id INTEGER PRIMARY KEY, \
                 parent_id INTEGER REFERENCES parent(id)\
             )",
            &[],
        )
        .unwrap();
        assert_eq!(
            eng.foreign_keys("child").unwrap()[0].ref_table,
            "app.parent"
        );
        eng.sql("INSERT INTO parent (id) VALUES (1)", &[]).unwrap();
        eng.sql("INSERT INTO child (id, parent_id) VALUES (10, 1)", &[])
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        reopened.foreign_keys("app.child").unwrap()[0].ref_table,
        "app.parent"
    );
    let error = reopened.sql("TRUNCATE app.parent", &[]).unwrap_err();
    assert!(error.to_string().contains("app.child"), "{error}");
    reopened.sql("TRUNCATE app.parent CASCADE", &[]).unwrap();
    assert!(reopened
        .sql("SELECT id FROM app.parent", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(reopened
        .sql("SELECT id FROM app.child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn create_table_with_a_missing_foreign_key_target_is_atomic_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("missing-fk.db");
    {
        let engine = Engine::open(&database).unwrap();
        let error = engine
            .sql(
                "CREATE TABLE orphan (\
                     id INTEGER PRIMARY KEY, \
                     embedding VECTOR(2), \
                     missing_id INTEGER REFERENCES missing(id)\
                 )",
                &[],
            )
            .unwrap_err();
        assert!(error.to_string().contains("missing"), "{error}");
        assert!(!engine.has_table("orphan").unwrap());

        engine
            .sql("CREATE TABLE orphan (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        assert!(engine.has_table("orphan").unwrap());
        engine.sql("DROP TABLE orphan", &[]).unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(!reopened.has_table("orphan").unwrap());
}

#[test]
fn legacy_unqualified_foreign_keys_ignore_search_path_and_fail_on_ambiguity() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-unqualified-fk.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine
            .sql("CREATE TABLE app.parent (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql(
                "CREATE TABLE app.child (\
                     id INTEGER PRIMARY KEY, \
                     parent_id INTEGER, \
                     FOREIGN KEY (parent_id) REFERENCES app.parent(id)\
                 )",
                &[],
            )
            .unwrap();
    }
    let connection = ManagedConnection::open(&database).unwrap();
    connection
        .with(|connection| {
            let constraints: String = connection.query_row(
                "SELECT constraints FROM _tables \
                 WHERE schema_name = 'app' AND relation_name = 'child'",
                [],
                |row| row.get(0),
            )?;
            assert!(constraints.contains("app.parent"), "{constraints}");
            let legacy = constraints.replace("app.parent", "parent");
            connection.execute(
                "UPDATE _tables SET constraints = ?1 \
                 WHERE schema_name = 'app' AND relation_name = 'child'",
                [legacy],
            )?;
            Ok(())
        })
        .unwrap();

    let reopened = Engine::open(&database).unwrap();
    reopened.sql("SET search_path TO public", &[]).unwrap();
    assert_eq!(
        reopened.foreign_keys("app.child").unwrap()[0].ref_table,
        "app.parent"
    );
    reopened
        .sql("INSERT INTO app.parent (id) VALUES (1)", &[])
        .unwrap();
    reopened
        .sql("INSERT INTO app.child (id, parent_id) VALUES (1, 1)", &[])
        .unwrap();

    reopened
        .sql("CREATE TABLE public.parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let snapshot_error = reopened.foreign_keys("app.child").unwrap_err();
    assert!(
        snapshot_error.to_string().contains("ambiguous persisted"),
        "{snapshot_error}"
    );
    let insert_error = reopened
        .sql("INSERT INTO app.child (id, parent_id) VALUES (2, 1)", &[])
        .unwrap_err();
    assert!(
        insert_error.to_string().contains("ambiguous persisted"),
        "{insert_error}"
    );
    drop(reopened);

    connection
        .with(|connection| {
            connection.execute(
                "UPDATE _tables \
                 SET constraints = replace(\
                     constraints, \
                     '\"ref_table\":\"parent\"', \
                     '\"ref_table\":\"missing\"'\
                 ) \
                 WHERE schema_name = 'app' AND relation_name = 'child'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let dangling = Engine::open(&database).unwrap();
    let dangling_error = dangling.foreign_keys("app.child").unwrap_err();
    assert!(
        dangling_error.to_string().contains("dangling persisted"),
        "{dangling_error}"
    );
}

#[test]
fn explain_runs_inner_statement_silently() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    // Plain EXPLAIN plans without executing the body.
    let explained = eng.sql("EXPLAIN SELECT * FROM t", &[]).unwrap();
    assert!(!explained.rows.is_empty());
}

#[test]
fn analyze_is_supported() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("ANALYZE", &[]).unwrap();
}

#[test]
fn transaction_begin_commit_round_trip() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('inside')", &[])
        .unwrap();
    eng.sql("COMMIT", &[]).unwrap();
    let res = eng.sql("SELECT body FROM t", &[]).unwrap();
    assert_eq!(res.rows.len(), 1);
}

#[test]
fn savepoint_release_round_trip() {
    let eng = Engine::new();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("SAVEPOINT sp", &[]).unwrap();
    eng.sql("RELEASE SAVEPOINT sp", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();
}
