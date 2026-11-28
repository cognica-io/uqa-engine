//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata maintenance and admission use the same durable transaction as source versions.

use super::*;

mod addressing;
mod wal;

#[test]
fn staged_versions_publish_their_fingerprint_in_one_metadata_insert() {
    use crate::mvcc::write::stage_record;
    use uqa_storage::{
        backup::RecordImageHasher, mvcc::CommitSequence, read_control::StorageReadControl,
    };

    let connection = Connection::open_in_memory().unwrap();
    schema::initialize(&connection).unwrap();
    let _permit = schema::WritePermit::acquire(&connection).unwrap();
    connection.execute_batch(
        "CREATE TEMP TABLE observations (kind TEXT, key BLOB, digest BLOB);
         CREATE TEMP TRIGGER observe_insert AFTER INSERT ON main._uqa_mvcc_version_metadata BEGIN INSERT INTO observations VALUES ('insert', NEW.key, NEW.image_digest); END;
         CREATE TEMP TRIGGER observe_update AFTER UPDATE ON main._uqa_mvcc_version_metadata BEGIN INSERT INTO observations VALUES ('update', NEW.key, NEW.image_digest); END;"
    ).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let sequence = CommitSequence::from_u64(1);
    let transaction = schema::begin(&connection).unwrap();
    for (key, value) in [
        (b"a", Some(b"value".as_slice())),
        (b"b", Some(b"")),
        (b"c", None),
    ] {
        stage_record(&transaction, key, value, sequence, &control).unwrap();
    }
    transaction.commit().unwrap();
    let observations = connection
        .prepare("SELECT kind, key, digest FROM observations ORDER BY rowid")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let digest = |value: &[u8]| {
        Some(
            RecordImageHasher::for_value(sequence, value, &control)
                .unwrap()
                .as_bytes()
                .to_vec(),
        )
    };
    assert_eq!(
        observations,
        [
            ("insert".into(), b"a".to_vec(), digest(b"value")),
            ("insert".into(), b"b".to_vec(), digest(b"")),
            ("insert".into(), b"c".to_vec(), None),
        ]
    );
}

fn lengths(connection: &Connection) -> Vec<(Vec<u8>, Vec<u8>, Option<i64>)> {
    connection
        .prepare("SELECT key, sequence, payload_length FROM _uqa_mvcc_version_metadata ORDER BY key, sequence")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn metadata_tracks_revisions_tombstones_rekeys_and_rollback() {
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize(&connection).unwrap();
    let _permit = schema::WritePermit::acquire(&connection).unwrap();
    let transaction = schema::begin(&connection).unwrap();
    transaction
        .execute_batch(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES
         (x'61', x'0000000000000001', x''),
         (x'61', x'0000000000000002', x'010203'),
         (x'61', x'0000000000000003', NULL);",
        )
        .unwrap();
    let expected = [(1, Some(0)), (2, Some(3)), (3, None)]
        .map(|(sequence, size)| (vec![b'a'], u64::to_be_bytes(sequence).to_vec(), size));
    assert_eq!(lengths(&transaction), expected);
    transaction.execute_batch(
        "SAVEPOINT preserved;
         UPDATE _uqa_mvcc_versions SET key = x'62', sequence = x'0000000000000004', value = x'ff' WHERE sequence = x'0000000000000002';
         DELETE FROM _uqa_mvcc_versions WHERE key = x'61';")
        .unwrap();
    assert_eq!(
        lengths(&transaction),
        [(vec![b'b'], 4_u64.to_be_bytes().to_vec(), Some(1))]
    );
    transaction
        .execute_batch("ROLLBACK TO preserved; RELEASE preserved")
        .unwrap();
    assert_eq!(lengths(&transaction), expected);
    transaction.rollback().unwrap();
    assert_eq!(lengths(&connection).len(), 0);
}

#[test]
fn version_metadata_failure_rolls_back_the_source_statement() {
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize(&connection).unwrap();
    let _permit = schema::WritePermit::acquire(&connection).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_metadata BEFORE INSERT ON _uqa_mvcc_version_metadata BEGIN SELECT RAISE(ABORT, 'metadata write failed'); END").unwrap();
    assert!(connection
        .execute_batch("INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (x'61', x'0000000000000001', x'ff')")
        .is_err());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM _uqa_mvcc_versions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(lengths(&connection).len(), 0);
}
