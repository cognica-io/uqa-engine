//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unbound physical readers keep data, auxiliary rows and their original allowance after capture.

use super::*;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn private_occurrences_survive_source_rollback() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index.add_document(1, fields([("body", "before")])).unwrap();
    index.conn.begin_transaction().unwrap();
    index
        .add_document(1, fields([("body", "private private")]))
        .unwrap();
    let captured = index.snapshot().unwrap();
    index.conn.rollback_transaction().unwrap();
    assert_eq!(index.doc_freq("body", "before").unwrap(), 1);
    assert_eq!(index.doc_freq("body", "private").unwrap(), 0);
    assert_eq!(captured.get_term_freq(1, "body", "private").unwrap(), 2);
    assert_eq!(captured.doc_freq("body", "before").unwrap(), 0);
    index.add_document(2, fields([("body", "after")])).unwrap();
    assert_eq!(captured.doc_count().unwrap(), 1);
}

#[rstest::rstest]
#[case::plain(false)]
#[case::encrypted(true)]
fn file_snapshots_survive_reopen_and_keep_empty_field_metadata(#[case] encrypted: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.db");
    let open = || {
        if encrypted {
            ManagedConnection::open_encrypted(&path, "snapshot test credential").unwrap()
        } else {
            ManagedConnection::open(&path).unwrap()
        }
    };
    let connection = open();
    Catalog::open(connection.clone()).unwrap();
    let mut index =
        SQLiteInvertedIndex::new(connection, "articles", uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields([("body", "alpha beta"), ("empty", "")]))
        .unwrap();
    let metadata = index.indexed_field_metadata(1, "empty").unwrap();
    let captured = index.snapshot().unwrap();
    drop(index);
    let connection = open();
    Catalog::open(connection.clone()).unwrap();
    let mut reopened =
        SQLiteInvertedIndex::new(connection, "articles", uqa_analysis::whitespace_analyzer());
    reopened.clear().unwrap();
    assert_eq!(reopened.doc_count().unwrap(), 0);
    assert_eq!(captured.doc_count().unwrap(), 1);
    assert_eq!(
        captured.indexed_field_metadata(1, "empty").unwrap(),
        metadata
    );
    assert_eq!(captured.get_doc_length(1, "body").unwrap(), 2);
    assert_eq!(
        captured.vocabulary_terms("body").unwrap(),
        vec!["alpha", "beta"]
    );
}

#[test]
fn copied_payloads_keep_the_capturing_allowance_and_release_on_last_reader() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let text = "alpha beta ".repeat(2048);
    index.add_document(1, fields([("body", &text)])).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = index.snapshot_with_control(&control).unwrap();
    let retained = control.memory().used();
    assert!(retained > 4096);
    let small = StorageReadControl::with_limit(retained / 2);
    assert!(matches!(
        index.snapshot_with_control(&small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(index.get_term_freq(1, "body", "alpha").unwrap(), 2048);
    let nested = captured
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(control.memory().used(), retained);
    index.clear().unwrap();
    drop((index, captured));
    assert_eq!(nested.get_term_freq(1, "body", "alpha").unwrap(), 2048);
    control.cancellation().cancel();
    assert!(matches!(
        nested.doc_count(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_and_cancelled_captures_do_not_follow_later_inserts() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let empty = index.snapshot().unwrap();
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(matches!(
        index.snapshot_with_control(&cancelled),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    index.add_document(1, fields([("body", "alpha")])).unwrap();
    assert_eq!(empty.doc_count().unwrap(), 0);
    assert_eq!(empty.doc_freq("body", "alpha").unwrap(), 0);
}

#[test]
fn captured_rebuild_marker_survives_source_reconstruction() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index.add_document(1, fields([("body", "alpha")])).unwrap();
    index.conn.with(|connection| {
        connection.execute("UPDATE _occurrence_formats SET format='source-rebuild' WHERE table_name='articles'", [])?;
        Ok(())
    }).unwrap();
    let legacy = index.snapshot().unwrap();
    index
        .try_rebuild_documents(vec![(1, fields([("body", "beta")]))])
        .unwrap();
    assert!(!index.source_rebuild_required().unwrap());
    assert!(legacy.source_rebuild_required().unwrap());
    assert!(legacy.get_posting_list("body", "alpha").is_err());
}

#[test]
fn captured_legacy_rows_without_a_marker_survive_source_reconstruction() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index.conn.with(|connection| {
        connection.execute("INSERT INTO _doc_lengths(table_name, doc_id, field, length) VALUES ('articles', 1, 'body', 2)", [])?;
        Ok(())
    }).unwrap();
    assert!(index.source_rebuild_required().unwrap());
    let legacy = index.snapshot().unwrap();
    index
        .try_rebuild_documents(vec![(1, fields([("body", "alpha")]))])
        .unwrap();
    assert!(!index.source_rebuild_required().unwrap());
    assert!(legacy.source_rebuild_required().unwrap());
    assert!(legacy.get_posting_list("body", "alpha").is_err());
}

struct Frequency;

impl BlockMaxScorer for Frequency {
    fn score(&self, term_freq: u64, _: u64, _: u64) -> f64 {
        term_freq as f64
    }
}

#[test]
fn captured_auxiliary_rows_keep_scores_and_quoted_field_names() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let field = "body_한글\"%";
    index
        .try_add_documents(
            (1..=257)
                .map(|id| (id, fields([(field, "alpha alpha beta")])))
                .collect(),
        )
        .unwrap();
    index.flush_skip_pointers().unwrap();
    index
        .build_block_max_scores_versioned(field, "alpha", &Frequency, "frequency-v1")
        .unwrap();
    let expected = index
        .persisted_block_max_scores(field, "alpha", "frequency-v1")
        .unwrap();
    assert_eq!(expected.as_ref().unwrap().len(), 3);
    let captured = index.snapshot().unwrap();
    index.clear().unwrap();
    assert_eq!(
        captured
            .persisted_block_max_scores(field, "alpha", "frequency-v1")
            .unwrap(),
        expected
    );
    assert_eq!(
        captured
            .persisted_block_max_scores(field, "alpha", "different")
            .unwrap(),
        None
    );
    assert_eq!(captured.doc_freq(field, "alpha").unwrap(), 257);
    let statistics = captured.field_stats(field).unwrap();
    assert_eq!(statistics.total_docs, 257);
    assert_eq!(statistics.avg_doc_length, 3.0);
    assert_eq!(statistics.doc_freq(field, "alpha"), 257);
    let mut cursor = captured.posting_cursor(field, "alpha").unwrap();
    assert_eq!(cursor.advance_to(200).unwrap().unwrap().doc_id, 200);
}

#[test]
fn malformed_capture_preserves_source_and_earlier_reader() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index.add_document(1, fields([("body", "alpha")])).unwrap();
    let captured = index.snapshot().unwrap();
    index.conn.with(|connection| {
        connection.execute("UPDATE _occurrence_clusters SET posting_count=posting_count+1 WHERE table_name='articles'", [])?;
        Ok(())
    }).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(index.snapshot_with_control(&control).is_err());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(captured.doc_freq("body", "alpha").unwrap(), 1);
    let count = index
        .conn
        .with(|connection| {
            Ok(connection.query_row(
                "SELECT posting_count FROM _occurrence_clusters WHERE table_name='articles'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn auxiliary_capture_selects_owned_fields_instead_of_a_table_name_prefix() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index.add_document(1, fields([("body", "alpha")])).unwrap();
    let mut other = SQLiteInvertedIndex::new(
        index.conn.clone(),
        "articles_extra",
        uqa_analysis::whitespace_analyzer(),
    );
    other.add_document(2, fields([("body", "beta")])).unwrap();
    other
        .build_block_max_scores_versioned("body", "beta", &Frequency, "frequency-v1")
        .unwrap();
    assert!(other
        .persisted_block_max_scores("body", "beta", "frequency-v1")
        .unwrap()
        .is_some());
    let captured = index.snapshot().unwrap();
    assert_eq!(
        captured
            .persisted_block_max_scores("extra_body", "beta", "frequency-v1")
            .unwrap(),
        None
    );
    assert_eq!(captured.doc_count().unwrap(), 1);
}
