//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_vector_command_refresh_preserves_generations_through_undo_and_reopen() {
    for mode in MODES {
        for kind in KINDS {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("commands.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            let mut left = index(&connection, kind, "docs");
            left.add(1, X.to_vec()).unwrap();
            left.initialize().unwrap();
            bind(&connection);
            let other = open(mode, &path);
            bind(&other);
            let mut right = index(&other, kind, "docs");
            connection.begin_transaction().unwrap();
            left.add(2, Y.to_vec()).unwrap();
            connection.savepoint("before").unwrap();
            let retained = left.snapshot().unwrap();
            right.add(3, Z.to_vec()).unwrap();
            connection
                .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
                .unwrap();
            assert_eq!(left.count().unwrap(), 3);
            assert_eq!(left.search_knn(&X, 100).unwrap().len(), 3);
            left.add(4, vec![0.25, 0.75, 0.0]).unwrap();
            right.add(5, vec![0.75, 0.25, 0.0]).unwrap();
            connection
                .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
                .unwrap();
            assert_eq!(left.count().unwrap(), 5);
            connection.rollback_to_savepoint("before").unwrap();
            assert_eq!(left.count().unwrap(), 2);
            connection
                .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
                .unwrap();
            assert_eq!(left.count().unwrap(), 4);
            right.add(6, vec![0.9, 0.1, 0.0]).unwrap();
            connection.commit_transaction().unwrap();
            assert_eq!(right.count().unwrap(), 5);
            assert_eq!(retained.count().unwrap(), 2);
            drop((left, right, retained, connection, other));
            let reopened = open(mode, &path);
            bind(&reopened);
            let index = index(&reopened, kind, "docs");
            assert_eq!(index.count().unwrap(), 5);
            assert_eq!(index.search_knn(&X, 100).unwrap().len(), 5);
        }
    }
}
