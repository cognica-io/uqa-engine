//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog::Catalog, key_value::SQLiteKeyValueStore, ManagedConnection};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::whitespace_analyzer;
use uqa_storage::{
    clustered_postings::PostingReadCursor, InvertedIndex, KeyValueInvertedIndex,
    MemoryInvertedIndex, StorageBackendError, StorageBackendResult,
};

fn native() -> SQLiteInvertedIndex {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    SQLiteInvertedIndex::new(connection, "docs", whitespace_analyzer())
}

fn values(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn populate(index: &mut dyn InvertedIndex, ids: &[u64]) {
    index
        .try_add_documents(
            ids.iter()
                .map(|id| (*id, values("alpha alpha beta")))
                .collect(),
        )
        .unwrap();
}

#[test]
fn native_and_key_value_sqlite_match_memory_scores_occurrences_and_seek_boundaries() {
    let ids: Vec<_> = (0..260)
        .chain([65536, 65538, 8 << 16, i64::MAX as u64])
        .collect();
    let mut providers: Vec<Box<dyn InvertedIndex>> = vec![
        Box::new(native()),
        Box::new(KeyValueInvertedIndex::new(
            Arc::new(SQLiteKeyValueStore::open_in_memory().unwrap()),
            "docs",
            whitespace_analyzer(),
        )),
    ];
    let mut memory = MemoryInvertedIndex::new(whitespace_analyzer());
    populate(&mut memory, &ids);
    let term = TokenTermKey::from_text("alpha");
    for provider in &mut providers {
        populate(provider.as_mut(), &ids);
        let control = StorageReadControl::with_limit(1 << 20);
        let other = control.memory().reserve(7).unwrap();
        {
            let mut expected = memory.posting_read_cursor_key("body", &term).unwrap();
            let mut actual = provider
                .posting_read_cursor_key_budgeted("body", &term, &control)
                .unwrap();
            assert_eq!(actual.doc_freq(), expected.doc_freq());
            while let Some(row) = expected.current() {
                assert_eq!(actual.current(), Some(row));
                if row.doc_id < 3 || row.doc_id >= 65536 {
                    let occurrences = provider
                        .get_occurrences_budgeted(row.doc_id, "body", &term, &control)
                        .unwrap();
                    assert_eq!(
                        &**occurrences,
                        &memory.get_occurrences(row.doc_id, "body", &term).unwrap()
                    );
                }
                actual.advance().unwrap();
                expected.advance().unwrap();
            }
            assert_eq!(actual.current(), None);
        }
        let expected = memory.field_stats_scalar("body").unwrap();
        let actual = provider
            .field_stats_scalar_budgeted("body", &control)
            .unwrap();
        assert_eq!(actual.total_docs, expected.total_docs);
        assert_eq!(actual.avg_doc_length, expected.avg_doc_length);
        {
            let mut cursor = provider
                .posting_read_cursor_key_budgeted("body", &term, &control)
                .unwrap();
            for target in [
                127,
                128,
                258,
                260,
                65537,
                8 << 16,
                i64::MAX as u64,
                u64::MAX,
            ] {
                let mut expected = memory.posting_read_cursor_key("body", &term).unwrap();
                assert_eq!(
                    cursor.advance_to(target).unwrap(),
                    expected.advance_to(target).unwrap()
                );
            }
        }
        assert_eq!(control.memory().used(), 7);
        drop(other);
    }
}

fn read(
    index: &dyn InvertedIndex,
    term: &TokenTermKey,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut cursor = index.posting_read_cursor_key_budgeted("body", term, control)?;
    let stats = index.field_stats_scalar_budgeted("body", control)?;
    assert_eq!(stats.total_docs, 2);
    let occurrences = index.get_occurrences_budgeted(0, "body", term, control)?;
    assert_eq!(occurrences.len(), 2);
    assert_eq!(cursor.advance_to(65536)?.unwrap().doc_id, 65536);
    Ok(())
}

#[test]
fn sqlite_provider_quota_failures_release_buffers_and_leave_autocommit_reads_reusable() {
    for mut provider in [
        Box::new(native()) as Box<dyn InvertedIndex>,
        Box::new(KeyValueInvertedIndex::new(
            Arc::new(SQLiteKeyValueStore::open_in_memory().unwrap()),
            "docs",
            whitespace_analyzer(),
        )),
    ] {
        populate(provider.as_mut(), &[0, 65536]);
        let term = TokenTermKey::from_text("alpha");
        let baseline = StorageReadControl::with_limit(1 << 20);
        let other = baseline.memory().reserve(7).unwrap();
        read(provider.as_ref(), &term, &baseline).unwrap();
        let peak = baseline.memory().peak();
        // Sample the complete interval and require the exact success boundary.
        for limit in (7..peak).step_by(7).chain([peak - 1, peak]) {
            let control = StorageReadControl::with_limit(limit);
            let unrelated = control.memory().reserve(7).unwrap();
            let result = read(provider.as_ref(), &term, &control);
            if limit < peak {
                assert!(
                    matches!(result, Err(StorageBackendError::Memory(_))),
                    "limit={limit}, peak={peak}: {result:?}"
                );
            } else {
                result.unwrap();
            }
            assert_eq!(control.memory().used(), 7);
            drop(unrelated);
        }
        assert_eq!(baseline.memory().used(), 7);
        let mut cursor = provider
            .posting_read_cursor_key_budgeted("body", &term, &baseline)
            .unwrap();
        let live = baseline.memory().used();
        let current = cursor.current();
        let hold = baseline
            .memory()
            .reserve(baseline.memory().limit() - live)
            .unwrap();
        assert!(matches!(
            cursor.advance(),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(cursor.current(), current);
        drop(hold);
        assert_eq!(baseline.memory().used(), live);
        baseline.cancellation().cancel();
        assert!(matches!(
            cursor.advance_to(65536),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(cursor.current(), current);
        baseline.cancellation().reset();
        assert_eq!(cursor.advance().unwrap().unwrap().doc_id, 65536);
        drop(cursor);
        assert_eq!(baseline.memory().used(), 7);
        read(provider.as_ref(), &term, &baseline).unwrap();
        drop(other);
    }
}

#[test]
fn native_cluster_visits_retain_cancellation_types_and_reject_late_corruption() {
    let mut index = native();
    populate(&mut index, &[0, 65536]);
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let mut calls = 0;
    assert!(matches!(
        index.visit_score_clusters("body", &term, None, usize::MAX, &control, &mut |_| {
            calls += 1;
            control.cancellation().cancel();
            Ok(())
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().reset();
    index
        .conn
        .with(|conn| {
            conn.execute(
                "UPDATE _occurrence_clusters SET cluster_id = -1 WHERE cluster_id = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(index
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .is_err());
    assert_eq!(control.memory().used(), 0);
    index
        .conn
        .with(|conn| {
            conn.execute(
                "UPDATE _occurrence_clusters SET cluster_id = 1 WHERE cluster_id = -1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    index.conn.with(|conn| {
        conn.execute("UPDATE _occurrence_clusters SET posting_count = posting_count + 1 WHERE cluster_id = 1", [])?;
        Ok(())
    }).unwrap();
    assert!(index
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .is_err());
    assert!(index
        .get_occurrences_budgeted(65536, "body", &term, &control)
        .is_err());
    assert_eq!(control.memory().used(), 0);
    index.conn.with(|conn| {
        conn.execute("UPDATE _occurrence_clusters SET posting_count = posting_count - 1 WHERE cluster_id = 1", [])?;
        conn.execute("UPDATE _occurrence_documents SET metadata_blob = zeroblob(65536) WHERE doc_id = 0", [])?;
        Ok(())
    }).unwrap();
    let small = StorageReadControl::with_limit(4096);
    let error = index
        .get_occurrences_budgeted(0, "body", &term, &small)
        .unwrap_err();
    assert!(
        !matches!(error, StorageBackendError::Memory(_)),
        "{error:?}"
    );
    assert_eq!(small.memory().used(), 0);
}

#[test]
fn lazy_native_clusters_and_occurrences_keep_the_callers_pinned_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("snapshot.sqlite3")).unwrap();
    Catalog::open(connection.clone()).unwrap();
    let reader = SQLiteInvertedIndex::new(connection.clone(), "docs", whitespace_analyzer());
    let mut writer =
        SQLiteInvertedIndex::new(connection.new_session(), "docs", whitespace_analyzer());
    populate(&mut writer, &[0, 65536]);
    connection.begin_deferred_transaction().unwrap();
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let mut cursor = reader
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    assert_eq!(cursor.doc_freq(), 2);
    writer.add_document(65536, values("replacement")).unwrap();
    assert_eq!(cursor.advance().unwrap().unwrap().doc_id, 65536);
    assert_eq!(
        reader
            .get_occurrences_budgeted(65536, "body", &term, &control)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        reader
            .field_stats_scalar_budgeted("body", &control)
            .unwrap()
            .avg_doc_length,
        3.0
    );
    drop(cursor);
    connection.commit_transaction().unwrap();
    assert_eq!(
        reader
            .posting_read_cursor_key_budgeted("body", &term, &control)
            .unwrap()
            .doc_freq(),
        1
    );
    assert!(reader
        .get_occurrences_budgeted(65536, "body", &term, &control)
        .unwrap()
        .is_empty());
    assert_eq!(control.memory().used(), 0);
}
