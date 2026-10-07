//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point metadata selects one historical version without scanning its history.

use super::*;

#[test]
fn point_metadata_preserves_historical_tombstones_and_head_diagnostics() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute_batch(
                "INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000000009', 0);
                 INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES
                 (x'61', x'0000000000000002', x'010203'),
                 (x'61', x'0000000000000005', NULL),
                 (x'61', x'0000000000000009', x'0102030405');",
            )?;
            for (boundary, expected) in [
                (0, None),
                (1, None),
                (2, Some((2, Some(3)))),
                (4, Some((2, Some(3)))),
                (5, Some((5, None))),
                (8, Some((5, None))),
                (9, Some((9, Some(5)))),
                (u64::MAX, Some((9, Some(5)))),
            ] {
                let found = read::info(sqlite, b"a", CommitSequence::from_u64(boundary))?;
                assert_eq!(found.map(|info| (info.revision, info.length)), expected);
                assert!(
                    read::info(sqlite, b"missing", CommitSequence::from_u64(boundary))?.is_none()
                );
            }
            // A still-present matching head must not hide a newer orphan version.
            sqlite.execute_batch(
                "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (x'61', x'000000000000000b', x'01');",
            )?;
            assert!(read::info(sqlite, b"a", CommitSequence::from_u64(11))
                .err()
                .unwrap()
                .to_string()
                .contains("record head has no matching version"));
            sqlite.execute_batch("DELETE FROM _uqa_mvcc_versions WHERE key = x'61'")?;
            assert!(read::info(sqlite, b"a", CommitSequence::from_u64(9))
                .err()
                .unwrap()
                .to_string()
                .contains("record head has no version"));
            assert!(read::info(sqlite, b"a", CommitSequence::from_u64(8))?.is_none());
            sqlite.execute_batch("UPDATE _uqa_mvcc_heads SET compacted = 1 WHERE key = x'61'")?;
            let compacted = read::info(sqlite, b"a", CommitSequence::from_u64(9))?.unwrap();
            assert_eq!((compacted.revision, compacted.length), (9, None));
            Ok(())
        })
        .unwrap();
}

#[test]
fn point_values_read_small_payloads_with_their_metadata_in_one_bounded_statement() {
    use rusqlite::StatementStatus;

    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let small = vec![7_u8; 3];
    let large = vec![9_u8; usize::from(read::INLINE_PAYLOAD_BYTES) + 1];
    store
        .with(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute_batch(
                "INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000000005', 0), (x'62', x'0000000000000002', 0);",
            )?;
            let mut insert = sqlite.prepare("INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, ?3)")?;
            for (key, sequence, value) in [
                (b"a", 2_u64, Some(small.as_slice())),
                (b"a", 5, None),
                (b"b", 2, Some(large.as_slice())),
            ] {
                insert.execute(params![key.as_slice(), sequence.to_be_bytes().as_slice(), value])?;
            }
            for (key, boundary, expected) in [
                (&b"a"[..], 1_u64, None),
                (b"a", 2, Some((2, Some(small.clone())))),
                (b"a", 4, Some((2, Some(small.clone())))),
                (b"a", 5, Some((5, None))),
                (b"b", 2, Some((2, Some(large.clone())))),
                (b"missing", 9, None),
            ] {
                let mut found = None;
                read::value(
                    sqlite,
                    key,
                    CommitSequence::from_u64(boundary),
                    &control,
                    &mut |record| {
                        found = record.map(|record| {
                            (
                                record.revision.unwrap().as_u64(),
                                record.value.map(<[u8]>::to_vec),
                            )
                        });
                        Ok(())
                    },
                )?;
                assert_eq!(found, expected, "{key:?} at {boundary}");
            }
            let mut statement = sqlite.prepare(read::POINT_VALUE_SQL)?;
            let mut rows = statement.query(params![
                b"a",
                4_u64.to_be_bytes().as_slice(),
                i64::from(read::INLINE_PAYLOAD_BYTES)
            ])?;
            assert_eq!(rows.next()?.unwrap().get::<_, Vec<u8>>(4)?, small);
            assert!(rows.next()?.is_none());
            drop(rows);
            assert_eq!(statement.get_status(StatementStatus::Sort), 0);
            assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn point_metadata_uses_bounded_instructions_for_long_version_histories() {
    use rusqlite::StatementStatus;

    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute_batch(
                "INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000001000', 0);",
            )?;
            let mut insert = sqlite
                .prepare("INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (x'61', ?1, zeroblob(16))")?;
            for sequence in 1_u64..=4096 {
                insert.execute([sequence.to_be_bytes().as_slice()])?;
            }
            for boundary in [0_u64, 1, 2048, 4096, u64::MAX] {
                let mut statement = sqlite.prepare(read::POINT_METADATA_SQL)?;
                let mut rows = statement.query(params![b"a", boundary.to_be_bytes().as_slice()])?;
                let row = rows.next()?.unwrap();
                assert_eq!(
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    (boundary != 0).then(|| boundary.min(4096).to_be_bytes().to_vec())
                );
                assert!(rows.next()?.is_none());
                drop(rows);
                assert_eq!(statement.get_status(StatementStatus::Sort), 0);
                assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
                assert!(
                    statement.get_status(StatementStatus::VmStep) < 80,
                    "one predecessor must not scan or materialize the version history"
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn ordered_reads_reject_a_newer_orphan_without_invalidating_the_historical_view() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let commit_key = |key: &[u8]| {
        let writes = [RecordWrite {
            key,
            expected: None,
            value: Some(b"original"),
        }];
        let commit = PreparedRecordCommit::new(&writes, &control).unwrap();
        let transaction = store.allocate_transaction(&control).unwrap();
        store.commit(transaction, &commit, &control).unwrap();
    };
    commit_key(b"ordered/a");
    let historical = store.snapshot(&control).unwrap();
    commit_key(b"ordered/z");
    let current = store.snapshot(&control).unwrap();
    let retained_bytes = control.memory().used();
    store
        .with(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute(
                "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, x'00')",
                params![
                    b"ordered/a",
                    current.sequence().as_u64().to_be_bytes().as_slice()
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let mut rows = 0;
    historical
        .visit_prefix(
            b"ordered/",
            None,
            usize::MAX,
            &control,
            &mut |key, record| {
                assert_eq!(key, b"ordered/a");
                assert_eq!(record.value, Some(b"original".as_slice()));
                rows += 1;
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(rows, 1);
    for result in [
        current.visit_value(b"ordered/a", &control, &mut |_| {
            panic!("a corrupt version must fail before delivery")
        }),
        current.visit_keys(b"ordered/", None, 1, &control, &mut |_, _| {
            panic!("a corrupt version must fail before key delivery")
        }),
        current.visit_prefix(b"ordered/", None, 1, &control, &mut |_, _| {
            panic!("a corrupt version must fail before payload delivery")
        }),
    ] {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("record head has no matching version"));
    }
    assert_eq!(control.memory().used(), retained_bytes);
    drop((historical, current));
    assert_eq!(control.memory().used(), 0);
}
