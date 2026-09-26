//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement and fixed transaction views must select matching rows and vector scores.

use super::{assert_search, kind, sessions, sql, Arc, Engine, Value};
use crate::tests::relation_lock_support::{after_tuple_wait, error};

fn setup(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int PRIMARY KEY, marker int DEFAULT 1, embedding tensor(2)); INSERT INTO diskann_docs(id,embedding) VALUES(1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0]]),(3,ARRAY[ARRAY[-1.0,0.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    assert_eq!(kind(engine), "diskann");
}

#[test]
fn diskann_sql_isolation_keeps_the_first_data_snapshot_and_private_undo() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ"] {
            let (_directory, first, peer) = sessions(provider);
            setup(&first);
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SAVEPOINT before_read"),
            );
            sql(
                &peer,
                "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=2",
            );
            assert_search(&first, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
            sql(&first, "SAVEPOINT private; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=3; INSERT INTO diskann_docs(id,embedding) VALUES(4,ARRAY[ARRAY[-1.0,0.0]])");
            sql(&peer, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=2; INSERT INTO diskann_docs(id,embedding) VALUES(5,ARRAY[ARRAY[1.0,0.0]])");
            if isolation == "READ COMMITTED" {
                assert_search(&first, &[(5, 1.0), (1, 0.0), (2, -1.0), (4, -1.0)]);
            } else {
                assert_search(&first, &[(2, 1.0), (1, 0.0), (4, -1.0)]);
            }
            for savepoint in ["private", "before_read"] {
                sql(&first, &format!("ROLLBACK TO {savepoint}"));
                if isolation == "READ COMMITTED" {
                    assert_search(&first, &[(1, 1.0), (5, 1.0), (2, -1.0), (3, -1.0)]);
                } else {
                    assert_search(&first, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
                }
            }
            sql(&first, "COMMIT");
            assert_search(&first, &[(1, 1.0), (5, 1.0), (2, -1.0), (3, -1.0)]);
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            drop((first, peer));
            let reopened = Engine::from_persistent_provider(factory).unwrap();
            assert_eq!(kind(&reopened), "diskann");
            assert_search(&reopened, &[(1, 1.0), (5, 1.0), (2, -1.0), (3, -1.0)]);
        }
    }
}

#[test]
fn diskann_sql_lock_wait_rechecks_the_current_tensor_and_isolation() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ"] {
            for change in ["score", "membership", "predicate", "delete"] {
                let (_directory, holder, waiter) = sessions(provider);
                setup(&holder);
                let doc_id = holder.table_doc_ids("diskann_docs").unwrap()[0];
                sql(&waiter, &format!("BEGIN ISOLATION LEVEL {isolation}"));
                sql(&holder, "BEGIN");
                sql(
                    &holder,
                    match change {
                        "score" => "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]] WHERE id=1",
                        "membership" => "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1",
                        "predicate" => "UPDATE diskann_docs SET marker=2,embedding=ARRAY[ARRAY[0.0,1.0]] WHERE id=1",
                        _ => "DELETE FROM diskann_docs WHERE id=1",
                    },
                );
                let top_k = if change == "membership" { 1 } else { 10 };
                let statement = format!("SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],{top_k}) AND id=1 AND marker=1 FOR UPDATE");
                let (waiter, result) = after_tuple_wait(
                    &holder,
                    waiter,
                    &statement,
                    "public.diskann_docs",
                    doc_id,
                    "COMMIT",
                );
                if isolation == "REPEATABLE READ" {
                    let failure = result.unwrap_err();
                    assert_eq!(failure.sqlstate(), Some("40001"), "{failure}");
                } else {
                    let result = result.unwrap();
                    if change == "score" {
                        assert_eq!(result.rows.len(), 1);
                        assert_eq!(result.rows[0]["id"], Value::Int(1));
                        assert_eq!(result.rows[0]["_score"], Value::Float(0.0));
                    } else {
                        assert!(result.rows.is_empty(), "{change}: {:?}", result.rows);
                    }
                }
                sql(&waiter, "ROLLBACK");
            }
        }
    }
}

#[test]
fn diskann_sql_failed_statement_preserves_prior_private_vectors_and_reopen() {
    for provider in 0..3 {
        let (_directory, first, peer) = sessions(provider);
        setup(&first);
        sql(&first, "BEGIN; INSERT INTO diskann_docs(id,embedding) VALUES(4,ARRAY[ARRAY[0.0,1.0]]); SAVEPOINT before_failure");
        error(
            &first,
            "UPDATE diskann_docs SET id=10,embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id IN (1,2)",
            "23505",
        );
        error(&first, "SELECT id FROM diskann_docs", "25P02");
        assert_search(&peer, &[(1, 1.0), (2, 0.0), (3, -1.0)]);
        sql(&first, "ROLLBACK TO before_failure");
        assert_search(&first, &[(1, 1.0), (2, 0.0), (4, 0.0), (3, -1.0)]);
        sql(&first, "COMMIT");
        assert_search(&peer, &[(1, 1.0), (2, 0.0), (4, 0.0), (3, -1.0)]);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(kind(&reopened), "diskann");
        assert_search(&reopened, &[(1, 1.0), (2, 0.0), (4, 0.0), (3, -1.0)]);
    }
}
