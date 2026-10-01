//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared deletion publishes complete text removals before SQL observers.

use uqa_core::Value;
use uqa_engine::Engine;

fn count(engine: &Engine, query: &str) -> i64 {
    let result = engine.sql(query, &[]).unwrap();
    match result.rows[0].get("n") {
        Some(Value::Int(value)) => *value,
        other => panic!("expected count, got {other:?}"),
    }
}

fn fixture(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO docs VALUES (1,'alpha'),(2,NULL),(3,'alpha'),(4,'alpha');
         CREATE INDEX docs_text ON docs USING gin(body) WITH(analyzer='whitespace')",
            &[],
        )
        .unwrap();
}

#[test]
fn deletion_publishes_text_before_triggers_and_restores_savepoint_versions() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delete.db");
        let engine = if persistent {
            Engine::open(&path).unwrap()
        } else {
            Engine::new()
        };
        fixture(&engine);
        engine.sql(
            "CREATE TABLE audit(n BIGINT);
             CREATE FUNCTION observe_delete() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
               INSERT INTO audit SELECT count(*) FROM docs WHERE fts_match(body,'alpha');
               RETURN OLD;
             END $$;
             CREATE TRIGGER observe_text AFTER DELETE ON docs FOR EACH ROW EXECUTE FUNCTION observe_delete()",
            &[],
        ).unwrap();
        assert_eq!(
            engine
                .sql("DELETE FROM docs WHERE id<=3 RETURNING id", &[])
                .unwrap()
                .rows
                .len(),
            3
        );
        assert_eq!(
            count(&engine, "SELECT count(*) AS n FROM audit WHERE n=1"),
            3
        );
        engine
            .sql("BEGIN; SAVEPOINT before_delete; DELETE FROM docs", &[])
            .unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
            ),
            0
        );
        engine
            .sql("ROLLBACK TO before_delete; COMMIT", &[])
            .unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
            ),
            1
        );
        if persistent {
            drop(engine);
            let reopened = Engine::open(&path).unwrap();
            assert_eq!(
                count(
                    &reopened,
                    "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
                ),
                1
            );
        }
    }
}

#[test]
fn deletion_trigger_failure_restores_rows_and_text() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = if persistent {
            Engine::open(&directory.path().join("failed.db")).unwrap()
        } else {
            Engine::new()
        };
        fixture(&engine);
        engine.sql(
            "CREATE FUNCTION reject_delete() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RAISE EXCEPTION 'reject deletion'; END $$;
             CREATE TRIGGER reject_text AFTER DELETE ON docs FOR EACH ROW EXECUTE FUNCTION reject_delete()",
            &[],
        ).unwrap();
        assert!(engine.sql("DELETE FROM docs", &[]).is_err());
        assert_eq!(count(&engine, "SELECT count(*) AS n FROM docs"), 4);
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
            ),
            3
        );
    }
}

#[test]
fn deletion_cascades_preserve_text_removal_in_both_relations() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = if persistent {
            Engine::open(&directory.path().join("cascade.db")).unwrap()
        } else {
            Engine::new()
        };
        engine.sql(
            "CREATE TABLE parents(id INTEGER PRIMARY KEY, body TEXT);
             CREATE TABLE children(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE, body TEXT);
             INSERT INTO parents VALUES(1,'alpha'),(2,'alpha');
             INSERT INTO children VALUES(1,1,'alpha'),(2,2,'alpha');
             CREATE INDEX parents_text ON parents USING gin(body) WITH(analyzer='whitespace');
             CREATE INDEX children_text ON children USING gin(body) WITH(analyzer='whitespace');
             DELETE FROM parents WHERE id=1",
            &[],
        ).unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM parents WHERE fts_match(body,'alpha')"
            ),
            1
        );
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM children WHERE fts_match(body,'alpha')"
            ),
            1
        );
    }
}
