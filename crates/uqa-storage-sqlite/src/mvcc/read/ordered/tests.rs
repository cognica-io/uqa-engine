//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered predecessor selection remains bounded by keys, not retained history depth.

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn connection() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY, sequence BLOB, compacted INTEGER) WITHOUT ROWID;
         CREATE TABLE _uqa_mvcc_versions (key BLOB, sequence BLOB, value BLOB, PRIMARY KEY (key, sequence)) WITHOUT ROWID;
         CREATE TABLE _uqa_mvcc_runs (unused INTEGER);"
    ).unwrap();
    connection
        .execute_batch(super::super::super::version_metadata::TABLE.1)
        .unwrap();
    super::super::super::version_metadata::create_triggers(&connection).unwrap();
    connection
}

#[test]
fn ordered_predecessors_do_not_scan_retained_history() {
    let connection = connection();
    connection
        .execute_batch("INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000001000', 0)")
        .unwrap();
    let mut insert = connection
        .prepare("INSERT INTO _uqa_mvcc_versions VALUES (x'61', ?1, zeroblob(16))")
        .unwrap();
    for sequence in 1_u64..=4096 {
        insert.execute([sequence.to_be_bytes().as_slice()]).unwrap();
    }
    let steps = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&steps);
    connection
        .progress_handler(
            1,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    for boundary in [0_u64, 1, 2048, 4096, u64::MAX] {
        let mut found = Vec::new();
        steps.store(0, Ordering::Relaxed);
        super::super::values::visit(
            &connection,
            b"a",
            None,
            1,
            CommitSequence::from_u64(boundary),
            &control,
            &mut |key, record| {
                found.push((
                    key.to_vec(),
                    record.revision.unwrap().as_u64(),
                    record.value.unwrap().to_vec(),
                ));
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(
            found,
            if boundary == 0 {
                Vec::new()
            } else {
                vec![(b"a".to_vec(), boundary.min(4096), vec![0; 16])]
            }
        );
        assert!(
            steps.load(Ordering::Relaxed) < 256,
            "one ordered predecessor must not scan its version history"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn ordered_payloads_align_invisible_heads_tombstones_and_compacted_records() {
    let connection = connection();
    connection
        .execute_batch(
            "INSERT INTO _uqa_mvcc_heads VALUES
           (x'61', x'0000000000000004', 0),
           (x'62', x'0000000000000002', 0),
           (x'63', x'0000000000000003', 0),
           (x'64', x'0000000000000002', 1),
           (x'65', x'0000000000000001', 0),
           (x'66', x'0000000000000004', 1);
         INSERT INTO _uqa_mvcc_versions VALUES
           (x'61', x'0000000000000004', x'61'),
           (x'62', x'0000000000000002', NULL),
           (x'63', x'0000000000000001', x'63'),
           (x'63', x'0000000000000003', x'6303'),
           (x'64', x'0000000000000001', zeroblob(1048576)),
           (x'65', x'0000000000000001', x'');",
        )
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let expected = vec![
        (b"b".to_vec(), 2, None),
        (b"c".to_vec(), 1, Some(b"c".to_vec())),
        (b"d".to_vec(), 2, None),
        (b"e".to_vec(), 1, Some(Vec::new())),
    ];
    for (after, limit, start, end) in [
        (None, usize::MAX, 0, 4),
        (None, 2, 0, 2),
        (Some(b"c".as_slice()), 2, 2, 4),
    ] {
        let mut found = Vec::new();
        super::super::values::visit(
            &connection,
            b"",
            after,
            limit,
            CommitSequence::from_u64(2),
            &control,
            &mut |key, record| {
                found.push((
                    key.to_vec(),
                    record.revision.unwrap().as_u64(),
                    record.value.map(<[u8]>::to_vec),
                ));
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(found, expected[start..end]);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() < 4096);
    }
}

#[test]
fn ordered_metadata_preserves_sequence_and_payload_corruption_errors() {
    let connection = connection();
    connection
        .execute_batch("INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000000001', 0)")
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    for (sequence, value, diagnostic) in [
        (
            rusqlite::types::Value::Text("bad".into()),
            rusqlite::types::Value::Null,
            "expected a record BLOB",
        ),
        (
            rusqlite::types::Value::Blob(vec![1]),
            rusqlite::types::Value::Null,
            "invalid sequence length",
        ),
        (
            rusqlite::types::Value::Blob(1_u64.to_be_bytes().to_vec()),
            rusqlite::types::Value::Integer(7),
            "encoded payload is not a BLOB",
        ),
        (
            rusqlite::types::Value::Blob(2_u64.to_be_bytes().to_vec()),
            rusqlite::types::Value::Integer(7),
            "record head has no matching version",
        ),
    ] {
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_versions VALUES (x'61', ?1, ?2)",
                params![sequence, value],
            )
            .unwrap();
        let error = visit(
            &connection,
            b"a",
            None,
            1,
            CommitSequence::from_u64(u64::MAX),
            &control,
            &mut |_, _| panic!("invalid metadata must not reach a visitor"),
        )
        .unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{error}");
        connection
            .execute_batch("DELETE FROM _uqa_mvcc_versions")
            .unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn predecessor_reads_do_not_open_payload_pages() {
    use crate::mvcc::{read, schema, SQLiteRecordStore};
    use crate::ManagedConnection;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute_batch(
                "INSERT INTO _uqa_mvcc_heads VALUES (x'61', x'0000000000000003', 0);
             INSERT INTO _uqa_mvcc_versions VALUES
             (x'61', x'0000000000000001', zeroblob(1048576)),
             (x'61', x'0000000000000002', NULL),
             (x'61', x'0000000000000003', x'');",
            )?;
            connection.authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "_uqa_mvcc_versions",
                        ..
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))?;
            let control = StorageReadControl::with_limit(64);
            for (sequence, expected) in [
                (0, None),
                (1, Some(Some(1_048_576))),
                (2, Some(None)),
                (3, Some(Some(0))),
            ] {
                let boundary = CommitSequence::from_u64(sequence);
                assert_eq!(
                    read::info(connection, b"a", boundary)?.map(|info| info.length),
                    expected
                );
                let mut lengths = Vec::new();
                read::ordered::visit(
                    connection,
                    b"a",
                    None,
                    1,
                    boundary,
                    &control,
                    &mut |key, info| {
                        assert_eq!(key, b"a");
                        lengths.push(info.length);
                        Ok(true)
                    },
                )?;
                assert_eq!(lengths, expected.into_iter().collect::<Vec<_>>());
                assert_eq!(control.memory().used(), 0);
            }
            connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
}
