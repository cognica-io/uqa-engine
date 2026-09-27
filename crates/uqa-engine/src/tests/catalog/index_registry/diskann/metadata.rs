//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{assert_search, sessions, sql, Engine, Value};

fn owners(mut run: impl FnMut(&Engine)) {
    run(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        drop(peer);
        run(&engine);
    }
}

#[test]
fn diskann_retained_query_validates_its_captured_column_after_rewrite_rollback() {
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        owners(|engine| {
            sql(engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
            sql(engine, "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM diskann_docs; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(3) USING ARRAY[0.0,1.0,0.0]");
            let snapshot = engine.capture_statement_read_snapshot().unwrap();
            let reader = engine.statement_read_snapshot_engine(&snapshot);
            sql(engine, "ROLLBACK");
            let found = sql(&reader, "SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[0.0,1.0,0.0],10) ORDER BY id");
            assert_eq!(
                found
                    .rows
                    .iter()
                    .map(|row| (&row["id"], &row["_score"]))
                    .collect::<Vec<_>>(),
                vec![
                    (&Value::Int(1), &Value::Float(1.0)),
                    (&Value::Int(2), &Value::Float(1.0))
                ]
            );
            let error = reader
                .sql(
                    "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],10)",
                    &[],
                )
                .unwrap_err();
            assert!(
                matches!(error, uqa_sql::SQLError::TypeMismatch(message) if message == "vector query for \"embedding\" has 2 dimensions, expected 3")
            );
            assert_search(engine, &[(1, 1.0), (2, 0.0)]);
        });
        finished.send(()).unwrap();
    });
    completion
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("column publication must not recursively read its own column write lock");
    worker.join().unwrap();
}

#[test]
fn retained_attention_uses_captured_statistics_after_live_table_drop() {
    owners(|engine| {
        sql(engine, "CREATE TABLE attention_docs(id int, title text, body text); CREATE INDEX attention_idx ON attention_docs USING gin(title,body); INSERT INTO attention_docs VALUES(1,'machine learning','neural network'),(2,'database','query engine'),(3,'machine','database')");
        let query = "SELECT id,_score FROM attention_docs WHERE fuse_attention(bayesian_match(title,'machine'),bayesian_match(body,'neural')) ORDER BY id";
        let expected = sql(engine, query);
        assert_eq!(expected.rows.len(), 2);
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        sql(engine, "DROP TABLE attention_docs");
        assert!(reader.try_query_has_table("attention_docs").unwrap());
        assert_eq!(
            sql(&reader, "SELECT id FROM attention_docs ORDER BY id")
                .rows
                .len(),
            3
        );
        assert_eq!(sql(&reader, query).rows, expected.rows);
    });
}
