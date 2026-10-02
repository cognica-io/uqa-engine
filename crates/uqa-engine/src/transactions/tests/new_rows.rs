//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A newly inserted row takes no lock of its own, and a row that others can see or that may be replaced still does.

use std::collections::BTreeMap;

use uqa_sql::ast::{LockStrength, LockWait};

use super::mutation_failures::persistent_engine;
use super::Engine;
use crate::Value;

/// Whether `session` can lock the row without waiting.
fn lockable(session: &Engine, table: &str, doc_id: u64) -> bool {
    match session.lock_row(
        table,
        doc_id,
        LockStrength::ForUpdate,
        LockWait::NoWait,
        table,
    ) {
        Ok(crate::row_locks::LockAcquire::Granted { .. }) => true,
        Ok(crate::row_locks::LockAcquire::Skipped) => false,
        Err(error) => {
            assert_eq!(error.sqlstate(), Some("55P03"), "{error}");
            false
        }
    }
}

fn count(engine: &Engine, table: &str) -> Value {
    engine
        .sql(&format!("SELECT count(*) AS n FROM {table}"), &[])
        .unwrap()
        .rows[0]["n"]
        .clone()
}

#[test]
fn a_new_row_takes_no_lock_of_its_own() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let writer = persistent_engine(provider, &directory.path().join("new-rows.db"));
        assert!(writer.versioned_backend_transactions(), "{provider}");
        writer
            .sql(
                "CREATE TABLE keyed (id INTEGER PRIMARY KEY, value INTEGER);
                 CREATE TABLE named (name TEXT);
                 INSERT INTO keyed VALUES (1, 1)",
                &[],
            )
            .unwrap();
        let other = writer.new_session().unwrap();
        writer
            .sql(
                "BEGIN;
                 INSERT INTO keyed VALUES (2, 2), (3, 3);
                 INSERT INTO named VALUES ('a');
                 DELETE FROM keyed WHERE id = 1",
                &[],
            )
            .unwrap();
        other.sql("BEGIN", &[]).unwrap();
        // No other transaction can see or name the rows the writer inserted, with a key it supplied or an identity that was generated, so nothing is locked for them.
        for (table, doc_id) in [("keyed", 2), ("keyed", 3), ("named", 1)] {
            assert!(
                lockable(&other, table, doc_id),
                "{provider} {table} {doc_id}"
            );
        }
        // The row it deleted is one that others still see, and stays locked.
        assert!(!lockable(&other, "keyed", 1), "{provider}");
        other.sql("ROLLBACK", &[]).unwrap();
        // A typed write may replace a row, so it locks the identity it names.
        writer
            .add_document(
                "keyed",
                9,
                BTreeMap::from([
                    ("id".into(), Value::Int(9)),
                    ("value".into(), Value::Int(9)),
                ]),
            )
            .unwrap();
        other.sql("BEGIN", &[]).unwrap();
        assert!(!lockable(&other, "keyed", 9), "{provider}");
        other.sql("ROLLBACK", &[]).unwrap();
        writer.sql("COMMIT", &[]).unwrap();
        assert_eq!(count(&other, "keyed"), Value::Int(3), "{provider}");
        assert_eq!(count(&other, "named"), Value::Int(1), "{provider}");
    }
}
