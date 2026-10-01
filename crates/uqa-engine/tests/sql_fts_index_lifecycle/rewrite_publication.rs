//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL publication keeps text visible before triggers and atomic with row rewrites.

use uqa_core::Value;
use uqa_engine::Engine;

fn count(engine: &Engine, query: &str) -> i64 {
    let rows = engine.sql(query, &[]).unwrap().rows;
    match rows[0].get("n") {
        Some(Value::Int(value)) => *value,
        other => panic!("expected count, got {other:?}"),
    }
}

fn fixture(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT, quantity INTEGER);
             INSERT INTO docs VALUES (1,'alpha',1),(2,NULL,2),(3,'alpha',3),(4,'alpha',4);
             CREATE INDEX docs_text ON docs USING gin(body) WITH(analyzer='whitespace')",
            &[],
        )
        .unwrap();
}

#[test]
fn rewrites_publish_text_before_row_triggers_and_keep_savepoint_versions() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rewrite.db");
        let engine = if persistent {
            Engine::open(&path).unwrap()
        } else {
            Engine::new()
        };
        fixture(&engine);
        engine
            .sql(
                "CREATE TABLE audit(n BIGINT);
                 CREATE FUNCTION observe_rewrite() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN
                   INSERT INTO audit SELECT count(*) FROM docs WHERE fts_match(body,'beta');
                   RETURN NEW;
                 END $$;
                 CREATE TRIGGER observe_text AFTER UPDATE ON docs FOR EACH ROW EXECUTE FUNCTION observe_rewrite();
                 UPDATE docs SET body='beta', quantity=quantity+10",
                &[],
            )
            .unwrap();
        assert_eq!(
            count(&engine, "SELECT count(*) AS n FROM audit WHERE n=4"),
            4
        );
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
            ),
            0
        );
        engine
            .sql(
                "BEGIN; SAVEPOINT before_null; UPDATE docs SET body=NULL",
                &[],
            )
            .unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'beta')"
            ),
            0
        );
        engine.sql("ROLLBACK TO before_null; COMMIT", &[]).unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'beta')"
            ),
            4
        );
        // The key-changing row must drain earlier rewrites before immediate publication.
        engine
            .sql(
                "UPDATE docs SET id=CASE WHEN id=2 THEN 12 ELSE id END, body='gamma'",
                &[],
            )
            .unwrap();
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'gamma')"
            ),
            4
        );
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'beta')"
            ),
            0
        );
        assert_eq!(
            count(&engine, "SELECT count(*) AS n FROM docs WHERE id=12"),
            1
        );
        if persistent {
            drop(engine);
            let reopened = Engine::open(&path).unwrap();
            assert_eq!(
                count(
                    &reopened,
                    "SELECT count(*) AS n FROM docs WHERE fts_match(body,'gamma')"
                ),
                4
            );
        }
    }
}

#[test]
fn trigger_failure_rolls_back_batched_text_and_row_versions() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = if persistent {
            Engine::open(&directory.path().join("failed.db")).unwrap()
        } else {
            Engine::new()
        };
        fixture(&engine);
        engine.sql(
            "CREATE FUNCTION reject_rewrite() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RAISE EXCEPTION 'reject rewrite'; END $$;
             CREATE TRIGGER reject_text AFTER UPDATE ON docs FOR EACH ROW EXECUTE FUNCTION reject_rewrite()",
            &[],
        ).unwrap();
        assert!(engine
            .sql("UPDATE docs SET body='beta', quantity=99", &[])
            .is_err());
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'alpha')"
            ),
            3
        );
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) AS n FROM docs WHERE fts_match(body,'beta')"
            ),
            0
        );
        assert_eq!(count(&engine, "SELECT sum(quantity) AS n FROM docs"), 10);
    }
}
