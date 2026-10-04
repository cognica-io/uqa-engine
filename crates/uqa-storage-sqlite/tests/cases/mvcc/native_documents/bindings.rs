//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A table's owner binding is read once at a committed snapshot and still follows the transaction's own changes.

use super::*;

#[test]
fn a_binding_follows_the_transactions_own_changes_at_one_snapshot() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bindings.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
        connection.begin_transaction().unwrap();
        // The table has no binding at this snapshot.
        assert!(!documents.contains_doc_id(1).unwrap(), "{mode:?}");
        assert_eq!(documents.get(1).unwrap(), None);
        connection.savepoint("unbound").unwrap();
        // The first write binds the table privately, which every later read of this transaction follows.
        documents.put(1, fields(1)).unwrap();
        assert!(documents.contains_doc_id(1).unwrap(), "{mode:?}");
        assert_eq!(documents.get(1).unwrap(), Some(fields(1)));
        connection.rollback_to_savepoint("unbound").unwrap();
        assert!(!documents.contains_doc_id(1).unwrap(), "{mode:?}");
        documents.put(2, fields(2)).unwrap();
        connection.commit_transaction().unwrap();

        connection.begin_transaction().unwrap();
        assert!(documents.contains_doc_id(2).unwrap(), "{mode:?}");
        assert!(!documents.contains_doc_id(1).unwrap(), "{mode:?}");
        // Another session binds a table after this transaction's snapshot.
        let other = open(mode, &path);
        other
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        SQLiteDocumentStore::new(other.clone(), "later")
            .put(5, fields(5))
            .unwrap();
        let later = SQLiteDocumentStore::new(connection.clone(), "later");
        assert!(!later.contains_doc_id(5).unwrap(), "{mode:?}");
        assert!(!later.contains_doc_id(5).unwrap(), "{mode:?}");
        connection
            .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
            .unwrap();
        assert!(later.contains_doc_id(5).unwrap(), "{mode:?}");
        assert!(documents.contains_doc_id(2).unwrap(), "{mode:?}");
        connection.commit_transaction().unwrap();
    }
}
