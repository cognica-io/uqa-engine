//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{assert_search, kind, sessions, sql};

#[test]
fn diskann_publication_cache_restore_failure_keeps_the_confirmed_commit() {
    for provider in 0..3 {
        for rebuild in [false, true] {
            let (_directory, first, peer) = sessions(provider);
            sql(&first, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0]])");
            if rebuild {
                sql(
                    &first,
                    "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
                );
            }
            assert_search(&peer, &[(1, 1.0), (2, 0.0)]);
            sql(
                &first,
                "BEGIN; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1",
            );
            sql(
                &first,
                if rebuild {
                    "ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE tensor(2)"
                } else {
                    "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)"
                },
            );
            first.commit().unwrap();
            assert_eq!(first.transaction_depth(), 0);
            assert!(first.pending_commit().is_none());
            let backend = first.storage.backend.as_ref().unwrap();
            let committed = backend.change_version().unwrap().unwrap();
            let control = first.query_retention_control().unwrap();
            let exhausted = control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())
                .unwrap();
            let error = first.synchronize_table_catalog().unwrap_err();
            assert_eq!(
                uqa_execution::storage_errors::storage_error("restore committed catalog", &error)
                    .sqlstate(),
                Some("53200"),
                "provider {provider}, rebuild {rebuild}: {error}"
            );
            assert_eq!(first.transaction_depth(), 0);
            assert!(first.pending_commit().is_none());
            drop(exhausted);
            assert_eq!(backend.change_version().unwrap(), Some(committed));
            first.synchronize_table_catalog().unwrap();
            assert_eq!(kind(&first), "diskann");
            assert_search(&first, &[(2, 0.0), (1, -1.0)]);
            assert_search(&peer, &[(2, 0.0), (1, -1.0)]);
            assert_eq!(kind(&peer), "diskann");
            assert_eq!(
                backend.change_version().unwrap(),
                Some(committed),
                "restoration cannot rebuild or republish the generation"
            );
        }
    }
}
