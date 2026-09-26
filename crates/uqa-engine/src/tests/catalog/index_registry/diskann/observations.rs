//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

mod unvisited;

#[test]
fn diskann_sql_serializable_snapshot_preserves_invocation_controls() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
        sql(&engine, "BEGIN ISOLATION LEVEL SERIALIZABLE");
        engine.prepare_serializable_transaction_snapshot().unwrap();
        let table = engine.try_table("diskann_docs").unwrap().unwrap();
        let read = engine
            .serializable_table_state_read(&table)
            .unwrap()
            .unwrap();
        let snapshot = table
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .snapshot()
            .unwrap();
        let observed = uqa_execution::serializable::vector::observe_snapshot(
            Some(&read),
            &table.columns.read(),
            "embedding",
            snapshot,
        )
        .unwrap();
        let empty = StorageReadControl::with_limit(0);
        assert_eq!(
            observed
                .search_knn_with_control(&[1.0, 0.0], 1, &empty)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            observed
                .search_threshold_with_control(&[1.0, 0.0], 0.5, &empty)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(empty.memory().used(), 0);
        assert!(observed
            .search_knn_with_control(&[1.0, 0.0], 0, &empty)
            .unwrap()
            .is_empty());
        let query = StorageReadControl::with_limit(1 << 20);
        let nested = observed.snapshot().unwrap();
        let owner = engine.query_retention_control().unwrap();
        let held = owner
            .memory()
            .reserve(owner.memory().limit() - owner.memory().used())
            .unwrap();
        assert!(matches!(
            nested.search_knn_with_control(&[1.0, 0.0], 1, &query),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_control(&[1.0, 0.0], 0.5, &query),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(query.memory().used(), 0);
        drop(held);
        assert_eq!(
            nested
                .search_knn_with_control(&[1.0, 0.0], 1, &query)
                .unwrap()
                .len(),
            1
        );
        query.cancellation().cancel();
        assert!(matches!(
            nested.search_knn_with_control(&[1.0, 0.0], 1, &query),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            nested.search_threshold_with_control(&[1.0, 0.0], 0.5, &query),
            Err(StorageBackendError::Cancelled(_))
        ));
        sql(&engine, "ROLLBACK");
    }
}

#[test]
fn diskann_sql_serializable_reads_cover_nonreturned_candidates_but_not_zero_k_or_explain() {
    for provider in 0..3 {
        for mode in ["zero", "explain", "search"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE diskann_docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[0.0,1.0]),(2,ARRAY[-1.0,0.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
            sql(&first, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            sql(&second, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            if mode == "zero" {
                assert!(first
                    .knn_search("diskann_docs", "embedding", [1.0, 0.0], 0)
                    .unwrap()
                    .is_empty());
            } else if mode == "explain" {
                sql(&first, "EXPLAIN SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)");
            } else {
                let found = sql(
                    &first,
                    "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
                );
                assert_eq!(found.rows.len(), 1);
                assert_eq!(found.rows[0]["id"], Value::Int(1));
            }
            sql(&second, "SELECT v FROM t");
            sql(&first, "UPDATE t SET v=2");
            sql(
                &second,
                "UPDATE diskann_docs SET embedding=ARRAY[1.0,0.0] WHERE id=2",
            );
            let outcomes = [first.commit(), second.commit()];
            if mode == "search" {
                assert!(
                    outcomes.iter().any(Result::is_err),
                    "candidate dependency cycle committed"
                );
                for error in outcomes.into_iter().filter_map(Result::err) {
                    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
                }
            } else {
                assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
            }
        }
    }
}
