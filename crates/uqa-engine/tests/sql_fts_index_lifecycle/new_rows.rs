//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A new row without indexed text leaves the text index alone, and the index follows the row once it has text.

use uqa_core::Value;
use uqa_engine::Engine;

fn matching(engine: &Engine, term: &str) -> Vec<i64> {
    let rows = engine
        .sql(
            &format!("SELECT id FROM docs WHERE fts_match(body,'{term}') ORDER BY id"),
            &[],
        )
        .unwrap()
        .rows;
    rows.iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("expected an integer id, got {other:?}"),
        })
        .collect()
}

#[test]
fn rows_inserted_without_indexed_text_join_the_index_when_they_get_text() {
    for persistent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new-rows.db");
        let open = || {
            if persistent {
                Engine::open(&path).unwrap()
            } else {
                Engine::new()
            }
        };
        let engine = open();
        engine
            .sql(
                "CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT, quantity INTEGER);
                 CREATE INDEX docs_text ON docs USING gin(body) WITH(analyzer='whitespace');
                 INSERT INTO docs VALUES (1,NULL,1),(2,'alpha beta',2);
                 INSERT INTO docs (id, quantity) SELECT g, g FROM generate_series(3, 6) AS g",
                &[],
            )
            .unwrap();
        assert_eq!(matching(&engine, "alpha"), [2]);
        // Rows that had no text are indexed by the statement that gives them some.
        engine
            .sql("UPDATE docs SET body='alpha gamma' WHERE id IN (1, 4)", &[])
            .unwrap();
        assert_eq!(matching(&engine, "alpha"), [1, 2, 4]);
        assert_eq!(matching(&engine, "gamma"), [1, 4]);
        // An identity reused in one transaction keeps none of the earlier row's postings.
        engine
            .sql(
                "BEGIN; DELETE FROM docs WHERE id = 2; INSERT INTO docs VALUES (2,NULL,20); COMMIT",
                &[],
            )
            .unwrap();
        assert_eq!(matching(&engine, "alpha"), [1, 4]);
        assert_eq!(matching(&engine, "beta"), Vec::<i64>::new());
        // A conflicting insert replaces the row, and with it the row's text.
        engine
            .sql(
                "INSERT INTO docs VALUES (1,NULL,10) ON CONFLICT (id) DO UPDATE SET body = EXCLUDED.body",
                &[],
            )
            .unwrap();
        assert_eq!(matching(&engine, "alpha"), [4]);
        engine
            .sql("INSERT INTO docs VALUES (7,'alpha',7)", &[])
            .unwrap();
        assert_eq!(matching(&engine, "alpha"), [4, 7]);
        if persistent {
            drop(engine);
            let reopened = open();
            assert_eq!(matching(&reopened, "alpha"), [4, 7]);
            assert_eq!(matching(&reopened, "gamma"), [4]);
        }
    }
}
