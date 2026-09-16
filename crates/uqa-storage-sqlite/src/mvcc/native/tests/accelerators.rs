//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Accelerator conversion preserves known owners and refuses ambiguous populated legacy names.

use super::{
    materialization::{records, with},
    persistence::connection,
    *,
};
use crate::{SQLiteInvertedIndex, SQLiteRecordStore};
use std::collections::BTreeMap;
use uqa_analysis::whitespace_analyzer;
use uqa_storage::{block_max_index::BlockMaxScorer, mvcc::VersionedPersistence, InvertedIndex};

struct Frequency;
impl BlockMaxScorer for Frequency {
    fn score(&self, frequency: u64, _: u64, _: u64) -> f64 {
        frequency as f64
    }
}

#[test]
fn populated_dynamic_accelerators_convert_with_canonical_rows_in_every_file_mode() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("accelerators-{mode}.db"));
        let connection = connection(&path, mode);
        Catalog::open(connection.clone()).unwrap();
        let mut index =
            SQLiteInvertedIndex::new(connection.clone(), "docs_é", whitespace_analyzer());
        index
            .try_add_documents(
                (0..300)
                    .map(|id| {
                        (
                            id,
                            BTreeMap::from([("field_\"_data".into(), "alpha alpha".into())]),
                        )
                    })
                    .collect(),
            )
            .unwrap();
        index.flush_skip_pointers().unwrap();
        index
            .build_block_max_scores_versioned("field_\"_data", "alpha", &Frequency, "frequency")
            .unwrap();
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        for family in [
            NativeRecordFamily::OccurrenceSkips,
            NativeRecordFamily::OccurrenceBlockMax,
        ] {
            let rows = records(&connection, &store, family, &control);
            assert_eq!(rows.len(), 3);
            for record in rows {
                let (_, row) = decode_record(record.key(), record.row(), &control).unwrap();
                assert_eq!(row[0], ValueRef::Text("docs_é".as_bytes()));
                assert_eq!(row[1], ValueRef::Text(b"field_\"_data"));
                if family == NativeRecordFamily::OccurrenceBlockMax {
                    assert_eq!(row[4], ValueRef::Real(2.0));
                    assert_eq!(row[5], ValueRef::Text(b"frequency"));
                }
            }
        }
        with(&connection, |sqlite| {
            assert_eq!(sqlite.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND (name GLOB '_skip_*' OR name GLOB '_blockmax_*')", [], |row| row.get::<_, i64>(0))?, 0);
            assert_eq!(
                sqlite.query_row("SELECT format FROM _uqa_mvcc_native_format", [], |row| row
                    .get::<_, i64>(
                    0
                ))?,
                5
            );
            Ok(())
        });
        let reopened = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(reopened.database_id(), store.database_id());
    }
}

#[test]
fn ambiguous_populated_accelerators_leave_the_entire_source_format_unchanged() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    for (table, field) in [("a_b", "c"), ("a", "b_c")] {
        let mut index = SQLiteInvertedIndex::new(connection.clone(), table, whitespace_analyzer());
        index
            .add_document(1, BTreeMap::from([(field.into(), "alpha".into())]))
            .unwrap();
        index.flush_skip_pointers().unwrap();
    }
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(SQLiteRecordStore::for_native(&connection, &control).is_err());
    connection.with(|sqlite| {
        assert_eq!(sqlite.query_row("SELECT value FROM _metadata WHERE key='schema_version'", [], |row| row.get::<_, String>(0))?, "48");
        assert_eq!(sqlite.query_row("SELECT count(*) FROM _skip_a_b_c", [], |row| row.get::<_, i64>(0))?, 1);
        assert_eq!(sqlite.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('_occurrence_skips','_occurrence_block_max','_uqa_mvcc_native_format','_uqa_mvcc_metadata')", [], |row| row.get::<_, i64>(0))?, 0);
        Ok(())
    }).unwrap();
}

#[test]
fn native_format_three_upgrade_preserves_original_records_and_commit_sequence() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteInvertedIndex::new(connection.clone(), "docs", whitespace_analyzer());
    index
        .add_document(1, BTreeMap::from([("body".into(), "alpha".into())]))
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let before = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let snapshot = before.snapshot(&control).unwrap();
    let original = NativeRecordFamily::all()
        .flat_map(|family| records(&connection, &before, family, &control))
        .collect::<Vec<_>>();
    with(&connection, |sqlite| {
        let _permit = crate::mvcc::schema::WritePermit::acquire(sqlite)?;
        let transaction = crate::mvcc::schema::begin(sqlite)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_occurrence_guards; DROP TABLE _occurrence_skips; DROP TABLE _occurrence_block_max; DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch("CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 3), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))")?;
        transaction.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1,3,49)", [])?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(
                &crate::mvcc::schema::trigger("_uqa_mvcc_native_format", action).1,
            )?;
        }
        transaction.commit()?;
        Ok(())
    });
    let after = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(after.database_id(), before.database_id());
    let current = after.snapshot(&control).unwrap();
    assert_eq!(current.sequence(), snapshot.sequence());
    for record in original {
        assert_eq!(
            &***current
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            record.row()
        );
        assert_eq!(
            &***snapshot
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            record.row()
        );
    }
}

#[test]
fn native_occurrence_projection_preserves_physical_integer_values() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let mut index = SQLiteInvertedIndex::new(connection.clone(), "docs", whitespace_analyzer());
    index
        .try_add_documents(
            (0..129)
                .map(|id| (id, BTreeMap::from([("body".into(), "alpha alpha".into())])))
                .collect(),
        )
        .unwrap();
    index.flush_skip_pointers().unwrap();
    with(&connection, |sqlite| {
        assert_eq!(
            sqlite.query_row(
                "SELECT min(length),max(length),count(*) FROM _occurrence_lengths",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                ))
            )?,
            (2, 2, 129)
        );
        assert_eq!(
            sqlite.query_row(
                "SELECT doc_count,total_length FROM _occurrence_fields",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )?,
            (129, 258)
        );
        assert_eq!(
            sqlite.query_row(
                "SELECT skip_offset FROM _occurrence_skips WHERE skip_doc_id=128",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            128
        );
        Ok(())
    });
}

#[test]
fn converted_mixed_scorer_fingerprints_never_return_a_partial_matching_prefix() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteInvertedIndex::new(connection.clone(), "docs", whitespace_analyzer());
    index
        .try_add_documents(
            (0..129)
                .map(|id| (id, BTreeMap::from([("body".into(), "alpha".into())])))
                .collect(),
        )
        .unwrap();
    index
        .rebuild_persisted_block_max("body", &Frequency, "first")
        .unwrap();
    connection
        .with_mut(|sqlite| {
            sqlite.execute(
                "UPDATE _blockmax_docs_body SET scorer_fingerprint='different' WHERE block_idx=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    assert_eq!(
        index
            .persisted_block_max_scores("body", "alpha", "first")
            .unwrap(),
        None
    );
}
