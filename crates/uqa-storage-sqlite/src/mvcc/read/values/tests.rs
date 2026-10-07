//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Latest payload joins preserve analytic results with less deterministic VM work.

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const PREDECESSOR: &str = "SELECT h.key, CASE WHEN h.sequence <= ?3 AND h.compacted = 1 THEN NULL
     ELSE (SELECT v.value FROM _uqa_mvcc_version_metadata m JOIN _uqa_mvcc_versions v
     ON v.version_id = m.version_id AND v.key = m.key AND v.sequence = m.sequence
     WHERE m.key = h.key AND m.sequence <= ?3 ORDER BY m.sequence DESC LIMIT 1) END
     FROM _uqa_mvcc_heads h WHERE h.key >= ?1 AND (?2 IS NULL) ORDER BY h.key";

#[test]
fn latest_payload_join_matches_predecessors_with_less_sqlite_work() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE _uqa_mvcc_heads(key BLOB PRIMARY KEY, sequence BLOB, compacted INTEGER) WITHOUT ROWID;
         CREATE TABLE _uqa_mvcc_versions(key BLOB, sequence BLOB, value BLOB, version_id INTEGER PRIMARY KEY)"
    ).unwrap();
    connection
        .execute_batch(crate::mvcc::version_metadata::TABLE.1)
        .unwrap();
    crate::mvcc::version_metadata::create_triggers(&connection).unwrap();
    let sequence = 1_u64.to_be_bytes();
    let mut expected = Vec::new();
    for id in 0_u64..1024 {
        let key = id.to_be_bytes();
        let value = (id % 7 != 0).then(|| (id + 1).to_be_bytes().to_vec());
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_heads VALUES(?1,?2,0)",
                params![key.as_slice(), sequence.as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES(?1,?2,?3)",
                params![key.as_slice(), sequence.as_slice(), value.as_deref()],
            )
            .unwrap();
        expected.push((key.to_vec(), value));
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
    let mut measured = Vec::new();
    for sql in [PREDECESSOR, payload_statement(false, false)] {
        steps.store(0, Ordering::Relaxed);
        let mut statement = connection.prepare(sql).unwrap();
        let found = statement
            .query_map(
                params![b"".as_slice(), None::<&[u8]>, sequence.as_slice()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(found, expected);
        measured.push(steps.load(Ordering::Relaxed));
    }
    assert!(
        measured[1] < measured[0],
        "join={}, predecessor={}",
        measured[1],
        measured[0]
    );
}

#[test]
fn wrong_payload_addresses_cannot_substitute_another_records_bytes() {
    use crate::mvcc::{read, schema};
    use rusqlite::params;
    use uqa_storage::{mvcc::CommitSequence, read_control::StorageReadControl};

    for length in [2, usize::from(read::INLINE_PAYLOAD_BYTES) + 1] {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize(&connection).unwrap();
        let _permit = schema::WritePermit::acquire(&connection).unwrap();
        for (key, byte) in [(b"a", 1_u8), (b"b", 2_u8)] {
            connection.execute(
                "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, x'0000000000000001', ?2)",
                params![key.as_slice(), vec![byte; length]],
            ).unwrap();
            connection
                .execute(
                    "INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, x'0000000000000001')",
                    [key.as_slice()],
                )
                .unwrap();
        }
        connection.execute_batch("UPDATE _uqa_mvcc_version_metadata SET version_id = (SELECT version_id FROM _uqa_mvcc_versions WHERE key = x'62') WHERE key = x'61'").unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let boundary = CommitSequence::from_u64(1);
        assert!(
            read::value(&connection, b"a", boundary, &control, &mut |_| {
                panic!("a corrupt physical address exposed another record")
            })
            .is_err()
        );
        assert!(super::visit(
            &connection,
            b"a",
            None,
            1,
            boundary,
            &control,
            &mut |_, _| panic!("a corrupt address escaped the ordered reader")
        )
        .is_err());
        assert_eq!(control.memory().used(), 0);
    }
}
