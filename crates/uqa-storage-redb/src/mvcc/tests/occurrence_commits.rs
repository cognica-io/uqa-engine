//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{fmt::Write as _, sync::Arc};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage::{InvertedIndex, KeyValueInvertedIndex, KeyValueStore};

#[test]
fn redb_occurrence_commit_batches_unchanged_snapshot_reads() {
    for terms in [64, 256, 1024] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("commit.redb");
        let options = VersionedSessionOptions {
            retained_bytes: 2 << 20,
        };
        let storage = crate::RedbStorage::open_with_options(&path, options).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let mut index =
            KeyValueInvertedIndex::new(store.clone(), "notes", uqa_analysis::whitespace_analyzer());
        let mut text = String::new();
        for id in 0..terms {
            write!(text, "term{id:05} ").unwrap();
        }
        index
            .add_document(1, [("body".into(), text.clone())].into())
            .unwrap();
        let retained = index.snapshot().unwrap();
        store.begin_transaction().unwrap();
        index
            .add_document(2, [("body".into(), text)].into())
            .unwrap();
        let before = crate::mvcc::read::READ_WINDOWS.get();
        store.commit_transaction().unwrap();
        let reads = crate::mvcc::read::READ_WINDOWS.get() - before;
        eprintln!("redb terms={terms} physical_read_admissions={reads}");
        assert!(reads <= 32, "{terms} terms required {reads} physical reads");
        assert_eq!(index.doc_count().unwrap(), 2);
        assert_eq!(index.total_field_length("body").unwrap(), 2 * terms);
        assert_eq!(retained.doc_count().unwrap(), 1);
        for id in 0..terms {
            for doc in [1, 2] {
                assert_eq!(
                    index
                        .get_term_freq(doc, "body", &format!("term{id:05}"))
                        .unwrap(),
                    1
                );
            }
        }
        store.begin_transaction().unwrap();
        index
            .add_document(3, [("body".into(), "discarded".into())].into())
            .unwrap();
        store.rollback_transaction().unwrap();
        assert_eq!(index.doc_count().unwrap(), 2);
        drop((retained, index, store, storage));
        let storage = crate::RedbStorage::open_with_options(&path, options).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let index = KeyValueInvertedIndex::new(store, "notes", uqa_analysis::whitespace_analyzer());
        assert_eq!(index.doc_count().unwrap(), 2);
        assert_eq!(index.total_field_length("body").unwrap(), 2 * terms);
        assert_eq!(index.get_term_freq(1, "body", "term00000").unwrap(), 1);
        assert_eq!(
            index
                .get_term_freq(2, "body", &format!("term{:05}", terms - 1))
                .unwrap(),
            1
        );
    }
}
