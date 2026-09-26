//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn each_owner(run: impl Fn(&Engine)) {
    let memory = Engine::new();
    create(&memory);
    run(&memory);
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        create(&engine);
        run(&engine);
        let query =
            "SELECT id, _score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)";
        let expected = sql(&engine, query).rows;
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(kind(&reopened), "diskann");
        assert_eq!(sql(&reopened, query).rows, expected);
    }
}

fn create(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
}

#[test]
fn diskann_sql_index_rename_keeps_writes_and_undo_bound_to_the_original_identity() {
    each_owner(|engine| {
        let original = super::super::definition(engine, "diskann_idx")
            .catalog
            .unwrap();
        sql(engine, "ALTER INDEX diskann_idx RENAME TO renamed_idx; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]]");
        assert_search(engine, &[(1, 0.0)]);
        assert_eq!(
            super::super::definition(engine, "renamed_idx")
                .catalog
                .unwrap(),
            original
        );
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER INDEX renamed_idx RENAME TO temporary_name; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]]");
        assert_search(engine, &[(1, -1.0)]);
        sql(
            engine,
            "ROLLBACK TO kept; COMMIT; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]]",
        );
        assert_search(engine, &[(1, 1.0)]);
    });
}

#[test]
fn diskann_sql_column_rename_preserves_index_kind_and_canonical_vectors() {
    each_owner(|engine| {
        sql(
            engine,
            "ALTER TABLE diskann_docs RENAME COLUMN embedding TO renamed",
        );
        let table = engine.try_table("diskann_docs").unwrap().unwrap();
        assert_eq!(
            table
                .vector_indexes
                .read()
                .get("renamed")
                .unwrap()
                .index_kind(),
            "diskann"
        );
        let result = sql(
            engine,
            "SELECT _score FROM diskann_docs WHERE knn_match(renamed,ARRAY[1.0,0.0],1)",
        );
        assert_eq!(result.rows[0]["_score"], Value::Float(1.0));
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs RENAME COLUMN renamed TO embedding; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]]");
        assert_search(engine, &[(1, -1.0)]);
        sql(engine, "ROLLBACK TO kept; COMMIT; ALTER TABLE diskann_docs RENAME COLUMN renamed TO embedding; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]]");
        assert_eq!(kind(engine), "diskann");
        assert_search(engine, &[(1, 0.0)]);
    });
}

#[test]
fn diskann_sql_table_rename_preserves_index_kind_and_catalog_undo() {
    each_owner(|engine| {
        sql(engine, "ALTER TABLE diskann_docs RENAME TO renamed_docs; UPDATE renamed_docs SET embedding=ARRAY[ARRAY[0.0,1.0]]");
        let result = sql(
            engine,
            "SELECT _score FROM renamed_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
        );
        assert_eq!(result.rows[0]["_score"], Value::Float(0.0));
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE renamed_docs RENAME TO diskann_docs; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]]");
        assert_search(engine, &[(1, -1.0)]);
        sql(
            engine,
            "ROLLBACK TO kept; COMMIT; ALTER TABLE renamed_docs RENAME TO diskann_docs",
        );
        assert_eq!(kind(engine), "diskann");
        assert_search(engine, &[(1, 0.0)]);
    });
}
