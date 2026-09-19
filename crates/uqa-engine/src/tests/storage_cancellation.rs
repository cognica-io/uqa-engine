//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL cancellation reaches autonomous writes without cancelling rollback or sibling sessions.

use super::*;
use uqa_execution::catalog::sequence::values::context::SequenceValueRuntime;

#[test]
fn query_and_autonomous_sequence_writes_share_cancellation_but_cleanup_and_siblings_do_not() {
    for provider in ["sqlite", "key_value", "redb"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cancel-writes.db");
        let engine = match provider {
            "sqlite" => Engine::open(&path).unwrap(),
            "key_value" => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            "redb" => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        engine
            .sql("CREATE TABLE items (key TEXT); CREATE SEQUENCE ids", &[])
            .unwrap();
        let sibling = engine.new_session().unwrap();
        engine
            .sql("BEGIN; INSERT INTO items VALUES ('discarded')", &[])
            .unwrap();
        let autonomous = SequenceValueRuntime::open_nontransactional_sequence_session(&engine)
            .unwrap()
            .unwrap();
        engine.cancel();
        assert!(matches!(
            engine.allocate_next_id("items"),
            Err(SQLError::Cancelled(_))
        ));
        assert!(matches!(
            engine.truncate_locked_table("items", false),
            Err(SQLError::Cancelled(_))
        ));
        assert!(engine
            .storage
            .backend
            .as_ref()
            .unwrap()
            .write_cancellation()
            .unwrap()
            .is_cancelled());
        assert!(autonomous
            .backend
            .write_cancellation()
            .unwrap()
            .is_cancelled());
        assert!(!sibling.is_cancelled());
        assert!(!sibling
            .storage
            .backend
            .as_ref()
            .unwrap()
            .write_cancellation()
            .unwrap()
            .is_cancelled());
        assert!(matches!(
            autonomous
                .backend
                .identifier_allocator()
                .unwrap()
                .allocate_identifiers(
                    b"cancelled",
                    uqa_storage::mvcc::IdentifierRequest::Observe(41)
                ),
            Err(StorageBackendError::Cancelled(_))
        ));
        engine.rollback().unwrap();
        engine.reset_cancellation();
        assert!(engine
            .sql("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(engine.nextval("ids").unwrap(), 1);
        sibling
            .sql("INSERT INTO items VALUES ('kept')", &[])
            .unwrap();
        assert_eq!(
            engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
            1
        );
    }
}
