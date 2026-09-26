//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sessions, sql, Arc, Engine, Value};

mod observations;
mod renaming;

fn assert_search(engine: &Engine, expected: &[(i64, f64)]) {
    let result = sql(engine, "SELECT id, _score FROM diskann_docs WHERE knn_match(embedding, ARRAY[1.0,0.0], 10) ORDER BY _score DESC, id");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| {
                let Value::Int(id) = row["id"] else {
                    panic!("id must be integer")
                };
                let Value::Float(score) = row["_score"] else {
                    panic!("score must be float")
                };
                (id, score.to_bits())
            })
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|&(id, score)| (id, score.to_bits()))
            .collect::<Vec<_>>()
    );
}

fn kind(engine: &Engine) -> String {
    engine
        .try_table("diskann_docs")
        .unwrap()
        .unwrap()
        .vector_indexes
        .read()
        .get("embedding")
        .unwrap()
        .index_kind()
        .to_owned()
}

fn exercise(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0],ARRAY[-1.0,0.0]]),(3,ARRAY[ARRAY[-1.0,0.0]])");
    let exact_kind = kind(engine);
    sql(
        engine,
        "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
    );
    assert_eq!(kind(engine), "diskann");
    assert_search(engine, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
    let row = engine.catalog_index("diskann_idx").unwrap().unwrap();
    let parameters: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&row.parameters_json).unwrap();
    assert_eq!(parameters["pq_bytes"], "2");
    assert_eq!(parameters["max_degree"], "64");
    sql(engine, "BEGIN; SAVEPOINT kept; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=2; INSERT INTO diskann_docs VALUES(4,ARRAY[ARRAY[0.0,1.0]])");
    assert_search(engine, &[(4, 0.0), (1, -1.0), (3, -1.0)]);
    sql(engine, "ROLLBACK TO kept; DROP INDEX diskann_idx");
    assert_eq!(kind(engine), exact_kind);
    assert_search(engine, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
    sql(engine, "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding); ROLLBACK TO kept; COMMIT");
    assert_eq!(kind(engine), "diskann");
    assert_search(engine, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
    sql(engine, "BEGIN; SAVEPOINT kept; TRUNCATE diskann_docs; INSERT INTO diskann_docs VALUES(9,ARRAY[ARRAY[0.0,1.0]])");
    assert_search(engine, &[(9, 0.0)]);
    sql(engine, "ROLLBACK TO kept; COMMIT");
    assert_search(engine, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
}

#[test]
fn diskann_sql_memory_uses_the_same_catalog_and_undo_path() {
    exercise(&Engine::new());
}

#[test]
fn diskann_sql_create_search_mutate_drop_undo_and_reopen() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        exercise(&first);
        assert_search(&second, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(kind(&reopened), "diskann");
        assert_search(&reopened, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
    }
}

fn exercise_definition_lifetimes(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0]])");
    sql(engine, "BEGIN; SAVEPOINT kept; INSERT INTO diskann_docs VALUES(2,ARRAY[ARRAY[0.0,1.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    assert_search(engine, &[(1, 1.0), (2, 0.0)]);
    sql(engine, "ROLLBACK TO kept; COMMIT");
    assert!(engine.catalog_index("diskann_idx").unwrap().is_none());
    assert_search(engine, &[(1, 1.0)]);
    sql(engine, "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding); BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs DROP COLUMN embedding; ROLLBACK TO kept; COMMIT");
    assert_eq!(kind(engine), "diskann");
    assert_search(engine, &[(1, 1.0)]);
    sql(engine, "BEGIN; SAVEPOINT kept; DROP TABLE diskann_docs; CREATE TABLE diskann_docs(id int, embedding tensor(2)); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding); INSERT INTO diskann_docs VALUES(2,ARRAY[ARRAY[0.0,1.0]])");
    assert_search(engine, &[(2, 0.0)]);
    sql(engine, "ROLLBACK TO kept; COMMIT");
    assert_search(engine, &[(1, 1.0)]);
    sql(
        engine,
        "TRUNCATE diskann_docs; INSERT INTO diskann_docs VALUES(3,ARRAY[ARRAY[-1.0,0.0]])",
    );
    assert_eq!(kind(engine), "diskann");
    assert_search(engine, &[(3, -1.0)]);
    sql(engine, "ALTER TABLE diskann_docs DROP COLUMN embedding; ALTER TABLE diskann_docs ADD COLUMN embedding tensor(2); UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]]; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    assert_search(engine, &[(3, 0.0)]);
    sql(engine, "DROP TABLE diskann_docs; CREATE TABLE diskann_docs(id int, embedding tensor(2)); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding); INSERT INTO diskann_docs VALUES(4,ARRAY[ARRAY[1.0,0.0]])");
    assert_search(engine, &[(4, 1.0)]);
}

#[test]
fn diskann_sql_definition_lifetimes_survive_commit_undo_and_recreation() {
    exercise_definition_lifetimes(&Engine::new());
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        exercise_definition_lifetimes(&first);
        assert_search(&second, &[(4, 1.0)]);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(kind(&reopened), "diskann");
        assert_search(&reopened, &[(4, 1.0)]);
    }
}
