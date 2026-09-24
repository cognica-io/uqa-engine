//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native occurrence projections retain transaction boundaries and publish complete physical rows.

use super::{open, MODES};
use std::{
    collections::BTreeMap,
    sync::{mpsc, Arc},
    time::Duration,
};
use uqa_analysis::{keyword_analyzer, whitespace_analyzer};
use uqa_storage::{
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, AnalyzerPhase, InvertedIndex,
    StorageBackendError, TokenTermKey,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

#[path = "native_occurrences/accelerators.rs"]
mod accelerators;

#[path = "native_occurrences/merging.rs"]
mod merging;

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}
fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}
fn index(connection: &ManagedConnection, table: &str) -> SQLiteInvertedIndex {
    SQLiteInvertedIndex::new(connection.clone(), table, whitespace_analyzer())
}
fn memory() -> ManagedConnection {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    connection
}

#[test]
fn evaluated_text_changes_preserve_native_terms_and_atomicity_in_every_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("observed-occurrences.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        connection.begin_transaction().unwrap();
        uqa_storage::key_value::conformance::verify_inverted_index_changes(&mut index(
            &connection,
            "observed",
        ))
        .unwrap();
        connection.rollback_transaction().unwrap();
        assert_eq!(index(&connection, "observed").doc_count().unwrap(), 0);
    }
}

#[test]
fn independent_native_occurrence_writers_commit_while_another_index_stays_private() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("occurrences.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            let mut a = index(&connection, "left日本語");
            let mut b = index(&connection, "right");
            a.add_document(1, fields("alpha alpha beta")).unwrap();
            b.add_document(1, fields("beta")).unwrap();
            bind(&connection);
            let baseline = a.snapshot().unwrap();
            connection.begin_transaction().unwrap();
            a.add_document(65_536, fields("alpha")).unwrap();
            connection.savepoint("keep").unwrap();
            let private = a.snapshot().unwrap();
            a.add_document(2, fields("beta beta")).unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let other = open(mode, &other_path);
                bind(&other);
                let mut b = index(&other, "right");
                other.begin_transaction().unwrap();
                b.add_document(2, fields("beta beta")).unwrap();
                other.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native occurrence writer did not finish: {mode:?}, {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_eq!(b.doc_count().unwrap(), 1);
            let (count, length) = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    (3, 6)
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    (1, 3)
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    (2, 4)
                }
            };
            assert_eq!(a.doc_count().unwrap(), count);
            assert_eq!(a.total_field_length("body").unwrap(), length);
            assert_eq!(b.doc_count().unwrap(), 2);
            assert_eq!(baseline.doc_count().unwrap(), 1);
            assert_eq!(private.doc_count().unwrap(), 2);
            assert_eq!(
                private
                    .get_scoring_inputs_bulk(
                        &[1, 65_536, 1],
                        "body",
                        &["alpha".into(), "beta".into()]
                    )
                    .unwrap(),
                vec![(3, vec![2, 1]), (1, vec![1, 0]), (3, vec![2, 1])]
            );
            drop((a, b, baseline, private, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            let a = index(&reopened, "left日本語");
            assert_eq!(a.doc_count().unwrap(), count);
            assert_eq!(a.total_field_length("body").unwrap(), length);
            assert_eq!(
                index(&reopened, "right")
                    .get_term_freq(2, "body", "beta")
                    .unwrap(),
                2
            );
        }
    }
}

#[test]
fn occurrence_snapshots_keep_discarded_branches_and_reject_all_mutation() {
    let connection = memory();
    let mut live = index(&connection, "docs\0日本語");
    let empty = live.snapshot().unwrap();
    live.add_document(1, fields("alpha alpha beta")).unwrap();
    let mut baseline = live.snapshot().unwrap();
    let nested = baseline.snapshot().unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("keep").unwrap();
    live.remove_document(1).unwrap();
    live.add_document(i64::MAX as u64, fields("private"))
        .unwrap();
    let discarded = live.snapshot().unwrap();
    connection.rollback_to_savepoint("keep").unwrap();
    live.add_document(2, fields("later")).unwrap();
    connection.commit_transaction().unwrap();
    live.clear().unwrap();
    drop(live);
    assert_eq!(empty.doc_count().unwrap(), 0);
    assert_eq!(discarded.get_total_doc_length(i64::MAX as u64).unwrap(), 1);
    assert_eq!(discarded.doc_freq("body", "alpha").unwrap(), 0);
    for saved in [&baseline, &nested] {
        assert_eq!(saved.get_total_term_freq(1, "alpha").unwrap(), 2);
        assert_eq!(
            saved.field_stats_scalar("body").unwrap().avg_doc_length,
            3.0
        );
        assert_eq!(saved.get_posting_list_any_field("alpha").unwrap().len(), 1);
        assert_eq!(saved.doc_freq_any_field("alpha").unwrap(), 1);
        assert_eq!(
            saved
                .get_term_freqs_bulk(&[1, 1, 9], "body", "alpha")
                .unwrap(),
            BTreeMap::from([(1, 2), (9, 0)])
        );
    }
    let baseline = Arc::get_mut(&mut baseline).unwrap();
    assert!(baseline.add_document(9, fields("forbidden")).is_err());
    assert!(baseline.remove_document(1).is_err());
    assert!(baseline.clear().is_err());
    assert!(baseline.try_rebuild_documents(vec![]).is_err());
    assert!(baseline
        .set_field_analyzer("body", keyword_analyzer(), AnalyzerPhase::Both)
        .is_err());
    assert_eq!(baseline.doc_count().unwrap(), 1);
}

#[test]
fn occurrence_materialization_failure_retries_the_original_complete_batch() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut live = index(&connection, "docs");
        live.add_document(1, fields("alpha alpha")).unwrap();
        let observer = connection.new_session();
        connection.begin_transaction().unwrap();
        live.try_add_documents(vec![
            (1, fields("beta")),
            (65_536, fields("beta beta beta")),
        ])
        .unwrap();
        let evaluated = live.snapshot().unwrap();
        observer.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_occurrence_publication BEFORE INSERT ON _occurrence_documents BEGIN SELECT RAISE(ABORT, 'injected occurrence publication failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert!(connection.in_transaction());
        assert!(live.add_document(9, fields("must not replay")).is_err());
        assert_eq!(
            index(&observer, "docs")
                .get_term_freq(1, "body", "alpha")
                .unwrap(),
            2
        );
        assert_eq!(evaluated.total_field_length("body").unwrap(), 4);
        observer
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER fail_occurrence_publication")?;
                Ok(())
            })
            .unwrap();
        index(&observer, "docs")
            .add_document(3, fields("alpha alpha alpha"))
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(index(&observer, "docs").doc_count().unwrap(), 3);
        assert_eq!(evaluated.total_field_length("body").unwrap(), 4);
        assert_eq!(
            index(&observer, "docs")
                .get_term_freq(65_536, "body", "beta")
                .unwrap(),
            3
        );
        assert_eq!(
            index(&observer, "docs")
                .get_term_freq(3, "body", "alpha")
                .unwrap(),
            3
        );
        drop((live, evaluated, observer, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(
            index(&reopened, "docs").total_field_length("body").unwrap(),
            7
        );
        assert_eq!(
            index(&reopened, "docs")
                .get_term_freq(3, "body", "alpha")
                .unwrap(),
            3
        );
    }
}

#[test]
fn occurrence_validation_errors_and_same_document_conflicts_publish_nothing_partial() {
    let connection = memory();
    let mut a = index(&connection, "docs");
    a.add_document(1, fields("alpha")).unwrap();
    assert!(a
        .try_add_documents(vec![(2, fields("private")), (u64::MAX, fields("invalid"))])
        .is_err());
    assert!(!connection.in_transaction());
    assert_eq!(a.doc_count().unwrap(), 1);
    let other = connection.new_session();
    let mut b = index(&other, "docs");
    connection.begin_transaction().unwrap();
    a.add_document(2, fields("alpha alpha")).unwrap();
    b.add_document(2, fields("alpha alpha alpha")).unwrap();
    assert!(connection.commit_transaction().is_err());
    assert_eq!(b.get_term_freq(2, "body", "alpha").unwrap(), 3);
    connection.rollback_transaction().unwrap();
    assert_eq!(a.total_field_length("body").unwrap(), 4);
    assert!(a
        .set_field_analyzer("body", keyword_analyzer(), AnalyzerPhase::Index)
        .is_err());
    a.set_field_analyzer("body", keyword_analyzer(), AnalyzerPhase::Search)
        .unwrap();
    a.rebuild_with_analyzer_revision(
        "body",
        keyword_analyzer().compile().unwrap(),
        AnalyzerPhase::Both,
        vec![(7, fields("one token"))],
    )
    .unwrap();
    assert_eq!(a.get_term_freq(7, "body", "one token").unwrap(), 1);
    assert_eq!(a.indexed_field_metadata(1, "body").unwrap(), None);
}

#[test]
fn controlled_occurrence_reads_release_allocations_on_quota_and_cancellation() {
    let connection = memory();
    let mut live = index(&connection, "docs");
    live.try_add_documents(
        (0..20)
            .map(|id| (id * 65_536, fields("alpha alpha beta")))
            .collect(),
    )
    .unwrap();
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let mut clusters = Vec::new();
    live.visit_score_clusters("body", &term, Some(3), 2, &control, &mut |cluster| {
        clusters.push(cluster.cluster_id);
        Ok(())
    })
    .unwrap();
    assert_eq!(clusters, vec![4, 5]);
    assert_eq!(control.memory().used(), 0);
    let tiny = StorageReadControl::with_limit(1);
    assert!(matches!(
        live.get_occurrences_budgeted(0, "body", &term, &tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        live.get_occurrences_budgeted(0, "body", &term, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    let mut visited = 0;
    live.for_each_term_freq("body", "alpha", &mut |_, _| {
        visited += 1;
        assert_eq!(live.doc_count().unwrap(), 20);
    })
    .unwrap();
    assert_eq!(visited, 20);
}

#[test]
fn native_source_rebuild_retires_legacy_and_corrupt_discarded_rows_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("INSERT INTO _occurrence_formats VALUES ('docs', 'source-rebuild'); INSERT INTO _occurrence_clusters VALUES ('docs', 'body', x'0061', 0, 99, x'00', x'00'); INSERT INTO _doc_lengths VALUES ('docs', 2, 'body', 1)")?;
        Ok(())
    }).unwrap();
    bind(&connection);
    let mut live = index(&connection, "docs");
    let legacy = live.snapshot().unwrap();
    assert!(live.source_rebuild_required().unwrap());
    assert!(live.get_posting_list("body", "alpha").is_err());
    let control = StorageReadControl::with_limit(4096);
    assert!(live.field_stats_scalar_budgeted("body", &control).is_err());
    assert_eq!(control.memory().used(), 0);
    connection.begin_transaction().unwrap();
    live.try_rebuild_documents(vec![(7, fields("alpha alpha"))])
        .unwrap();
    assert!(!live.source_rebuild_required().unwrap());
    connection.rollback_transaction().unwrap();
    assert!(live.source_rebuild_required().unwrap());
    live.try_rebuild_documents(vec![(7, fields("alpha alpha"))])
        .unwrap();
    assert_eq!(live.get_term_freq(7, "body", "alpha").unwrap(), 2);
    assert!(legacy.source_rebuild_required().unwrap());
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _doc_lengths WHERE table_name='docs'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT posting_count FROM _occurrence_clusters WHERE table_name='docs'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_occurrence_snapshots_do_not_copy_the_stored_corpus() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retained.db");
    let writer = ManagedConnection::open(&path).unwrap();
    Catalog::open(writer.clone()).unwrap();
    let mut live = index(&writer, "docs");
    live.try_add_documents(
        (0..64)
            .map(|id| (id * 65_536, fields("alpha alpha beta")))
            .collect(),
    )
    .unwrap();
    bind(&writer);
    let reader = ManagedConnection::open(&path).unwrap();
    reader
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 4096,
        })
        .unwrap();
    let saved = index(&reader, "docs").snapshot().unwrap();
    let nested = saved.snapshot().unwrap();
    live.clear().unwrap();
    assert_eq!(saved.field_stats_scalar("body").unwrap().total_docs, 64);
    assert_eq!(nested.field_stats_scalar("body").unwrap().total_docs, 64);
    assert_eq!(live.doc_count().unwrap(), 0);
}

#[test]
fn native_controlled_cursors_preserve_committed_and_private_clusters_between_pages() {
    use uqa_storage::clustered_postings::PostingReadCursor;
    let connection = memory();
    let mut live = index(&connection, "docs");
    live.try_add_documents(vec![
        (1, fields("alpha")),
        (65_536, fields("alpha alpha")),
        (131_072, fields("alpha alpha alpha")),
    ])
    .unwrap();
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let mut cursor = live
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    let mut sibling = index(&connection.new_session(), "docs");
    sibling
        .try_rebuild_documents(vec![
            (1, fields("alpha")),
            (65_536, fields("alpha alpha alpha alpha")),
        ])
        .unwrap();
    assert_eq!(cursor.doc_freq(), 3);
    assert_eq!(cursor.advance().unwrap().unwrap().term_freq, 2);
    assert_eq!(cursor.advance_to(131_072).unwrap().unwrap().term_freq, 3);
    assert_eq!(cursor.advance().unwrap(), None);
    drop(cursor);
    assert_eq!(control.memory().used(), 0);
    connection.begin_transaction().unwrap();
    live.add_document(196_608, fields("alpha alpha alpha"))
        .unwrap();
    let mut cursor = live
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    connection.rollback_transaction().unwrap();
    assert_eq!(cursor.advance_to(196_608).unwrap().unwrap().term_freq, 3);
    drop(cursor);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(live.get_term_freq(196_608, "body", "alpha").unwrap(), 0);
}
