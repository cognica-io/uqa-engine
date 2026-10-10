//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole-index replacement retains its original structural conditions through refresh and publication.

use super::Persistence;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::CancellationToken;
use uqa_storage::inverted_index::TextIndexDocuments;
use uqa_storage::mvcc::VersionError;
use uqa_storage::{InvertedIndex, KeyValueInvertedIndex, KeyValueStore, StorageBackendError};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

#[test]
fn complete_occurrence_rebuild_does_not_reenter_delta_resolution() {
    for count in [16_u64, 1024] {
        let persistence = Persistence::new();
        let session = Arc::new(persistence.session(512 << 10));
        let peer = persistence.session(512 << 10);
        let mut index = KeyValueInvertedIndex::new(
            session.clone(),
            "docs",
            uqa_analysis::whitespace_analyzer(),
        );
        index.add_document(2048, fields("previous")).unwrap();
        let reader = index.snapshot().unwrap();
        session.begin_transaction().unwrap();
        session.savepoint("before").unwrap();
        for rollback in [true, false] {
            index
                .try_rebuild_documents(&mut TextIndexDocuments::new(
                    (0..count)
                        .map(|id| (id, fields("alpha beta alpha")))
                        .collect(),
                ))
                .unwrap();
            persistence.state.lock().occurrence_resolutions = 0;
            peer.put(b"unrelated", if rollback { b"one" } else { b"two" })
                .unwrap();
            session
                .refresh_transaction_snapshot(&CancellationToken::new())
                .unwrap();
            assert_eq!(
                persistence.state.lock().occurrence_resolutions,
                0,
                "a complete replacement has no document delta to merge"
            );
            assert_eq!(index.doc_count().unwrap(), count);
            assert_eq!(index.total_field_length("body").unwrap(), count * 3);
            assert_eq!(index.get_term_freq(count - 1, "body", "alpha").unwrap(), 2);
            assert_eq!(index.get_term_freq(2048, "body", "previous").unwrap(), 0);
            assert_eq!(reader.doc_count().unwrap(), 1);
            if rollback {
                session.rollback_to_savepoint("before").unwrap();
                assert_eq!(index.get_term_freq(2048, "body", "previous").unwrap(), 1);
            }
        }
        session.release_savepoint("before").unwrap();
        session.commit_transaction().unwrap();
        assert_eq!(persistence.state.lock().occurrence_resolutions, 0);
        let reopened =
            KeyValueInvertedIndex::new(Arc::new(peer), "docs", uqa_analysis::whitespace_analyzer());
        assert_eq!(reopened.doc_count().unwrap(), count);
        assert_eq!(
            reopened.get_term_freq(count - 1, "body", "alpha").unwrap(),
            2
        );
        drop((reader, reopened, index));
        assert_eq!(session.retention_control().memory().used(), 0);
    }
}

#[test]
fn complete_occurrence_rebuild_keeps_concurrent_source_conflicts() {
    for rebuild_first in [false, true] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 20));
        let b = Arc::new(persistence.session(1 << 20));
        let mut left =
            KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
        let mut right =
            KeyValueInvertedIndex::new(b.clone(), "docs", uqa_analysis::whitespace_analyzer());
        left.add_document(1, fields("original")).unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        left.try_rebuild_documents(&mut TextIndexDocuments::new(vec![(
            2,
            fields("rebuilt rebuilt"),
        )]))
        .unwrap();
        right.add_document(3, fields("concurrent")).unwrap();
        let (winner, loser) = if rebuild_first { (&a, &b) } else { (&b, &a) };
        winner.commit_transaction().unwrap();
        for error in [
            loser
                .refresh_transaction_snapshot(&CancellationToken::new())
                .unwrap_err(),
            loser.commit_transaction().unwrap_err(),
        ] {
            let StorageBackendError::Backend { source, .. } = error else {
                panic!("expected a typed MVCC write conflict");
            };
            assert!(matches!(
                source.downcast_ref::<VersionError>(),
                Some(VersionError::WriteConflict { .. })
            ));
        }
        loser.rollback_transaction().unwrap();
        assert_eq!(left.doc_count().unwrap(), if rebuild_first { 1 } else { 2 });
        assert_eq!(
            right.get_term_freq(2, "body", "rebuilt").unwrap(),
            if rebuild_first { 2 } else { 0 }
        );
        assert_eq!(
            right.get_term_freq(3, "body", "concurrent").unwrap(),
            u64::from(!rebuild_first)
        );
        assert_eq!(a.retention_control().memory().used(), 0);
        assert_eq!(b.retention_control().memory().used(), 0);
    }
}
