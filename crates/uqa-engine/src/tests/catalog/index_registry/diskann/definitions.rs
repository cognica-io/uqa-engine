//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn owners(mut run: impl FnMut(&Engine)) {
    run(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        drop(peer);
        run(&engine);
    }
}

fn create(engine: &Engine, indexed: bool) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0])");
    if indexed {
        sql(
            engine,
            "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
        );
    }
    sql(
        engine,
        "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM diskann_docs",
    );
}

fn copied(engine: &Engine) -> Engine {
    let snapshot = engine.capture_statement_read_snapshot().unwrap();
    let reader = engine.statement_read_snapshot_engine(&snapshot);
    assert_eq!(
        reader
            .require_query_table("diskann_docs")
            .unwrap()
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .index_kind(),
        "diskann"
    );
    reader
}

#[test]
fn diskann_complete_row_rewrite_retains_an_index_created_after_snapshot_capture() {
    owners(|engine| {
        create(engine, false);
        sql(engine, "UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0]");
        sql(
            engine,
            "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
        );
        let reader = copied(engine);
        sql(engine, "ROLLBACK");
        assert_search(&reader, &[(1, -1.0), (2, -1.0)]);
    });
}

#[test]
fn diskann_complete_row_rewrite_retains_a_replaced_index_definition() {
    owners(|engine| {
        create(engine, true);
        sql(engine, "UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0]");
        sql(engine, "DROP INDEX diskann_idx; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(max_degree=2,search_list_size=2,beam_width=1)");
        let reader = copied(engine);
        sql(engine, "ROLLBACK");
        assert_search(&reader, &[(1, -1.0), (2, -1.0)]);
    });
}

#[test]
fn diskann_fixed_copy_retains_a_rewritten_vector_column() {
    owners(|engine| {
        create(engine, true);
        sql(engine, "ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(3) USING ARRAY[0.0,1.0,0.0]");
        let reader = copied(engine);
        sql(engine, "ROLLBACK");
        let nested = copied(&reader);
        drop(reader);
        let found = sql(&nested, "SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[0.0,1.0,0.0],10) ORDER BY id");
        assert_eq!(found.rows.len(), 2);
        assert!(found
            .rows
            .iter()
            .all(|row| row["_score"] == Value::Float(1.0)));
    });
}

#[test]
fn diskann_index_creation_preserves_fixed_query_rows_after_concurrent_writes() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        create(&engine, false);
        sql(&peer, "UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1; DELETE FROM diskann_docs WHERE id=2; INSERT INTO diskann_docs VALUES(3,ARRAY[1.0,0.0])");
        sql(
            &engine,
            "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
        );
        {
            let table = engine.try_table("diskann_docs").unwrap().unwrap();
            let indexes = table.vector_indexes.read();
            let found = indexes
                .get("embedding")
                .unwrap()
                .search_knn(&[1.0, 0.0], 10);
            assert_eq!(
                found
                    .unwrap()
                    .iter()
                    .map(|entry| (entry.doc_id, entry.payload.score))
                    .collect::<Vec<_>>(),
                [(1, -1.0), (3, 1.0)]
            );
        }
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        sql(&engine, "ROLLBACK");
        assert_search(&reader, &[(1, 1.0), (2, 0.0)]);
    }
}
