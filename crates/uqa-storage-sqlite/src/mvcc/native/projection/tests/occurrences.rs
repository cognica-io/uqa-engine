//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt::Write as _;

use super::*;
use crate::SQLiteInvertedIndex;
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage::InvertedIndex;

fn fields<const N: usize>(pairs: [(&str, &str); N]) -> std::collections::BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
}

#[test]
fn native_occurrence_commit_batches_unchanged_snapshot_reads() {
    for terms in [64, 256, 1024] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("commit.db");
        let connection = ManagedConnection::open(&path).unwrap();
        Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions {
                retained_bytes: 2 << 20,
            })
            .unwrap();
        let mut index = SQLiteInvertedIndex::new(
            connection.clone(),
            "notes",
            uqa_analysis::whitespace_analyzer(),
        );
        let mut text = String::new();
        for id in 0..terms {
            write!(text, "term{id:05} ").unwrap();
        }
        index.add_document(1, fields([("body", &text)])).unwrap();
        let retained = index.snapshot().unwrap();
        connection.begin_transaction().unwrap();
        index.add_document(2, fields([("body", &text)])).unwrap();
        let reads = crate::mvcc::RECORD_READS.get();
        connection.commit_transaction().unwrap();
        let reads = crate::mvcc::RECORD_READS.get() - reads;
        eprintln!("terms={terms} physical_read_admissions={reads}");
        assert!(reads <= 32, "occurrence validation must share admission: {terms} terms required {reads} physical reads");
        assert_eq!(index.doc_count().unwrap(), 2);
        assert_eq!(index.total_field_length("body").unwrap(), 2 * terms);
        assert_eq!(retained.doc_count().unwrap(), 1);
        for id in 0..terms {
            assert_eq!(
                index
                    .get_term_freq(1, "body", &format!("term{id:05}"))
                    .unwrap(),
                1
            );
            assert_eq!(
                index
                    .get_term_freq(2, "body", &format!("term{id:05}"))
                    .unwrap(),
                1
            );
        }
        connection.begin_transaction().unwrap();
        index
            .add_document(3, fields([("body", "discarded")]))
            .unwrap();
        connection.rollback_transaction().unwrap();
        assert_eq!(index.doc_count().unwrap(), 2);
        drop((retained, index, connection));
        let connection = ManagedConnection::open(&path).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions {
                retained_bytes: 2 << 20,
            })
            .unwrap();
        let index =
            SQLiteInvertedIndex::new(connection, "notes", uqa_analysis::whitespace_analyzer());
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
