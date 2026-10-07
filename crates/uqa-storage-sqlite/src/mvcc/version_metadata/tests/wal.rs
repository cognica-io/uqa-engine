//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic WAL work for the provider's version publication boundary.

use super::*;
use crate::mvcc::write;
use rusqlite::params;
use uqa_storage::{mvcc::CommitSequence, read_control::StorageReadControl};

const FAMILIES: u8 = 6;
const EXISTING: u64 = 2048;
const COMMITS: u64 = 128;

fn key(family: u8, id: u64) -> [u8; 41] {
    let mut key = [0; 41];
    key[0] = family;
    key[33..].copy_from_slice(&id.to_be_bytes());
    key
}

fn frames(addressed: bool) -> u64 {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("versions.db");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA page_size = 4096; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 0").unwrap();
    schema::initialize(&connection).unwrap();
    let _permit = schema::WritePermit::acquire(&connection).unwrap();
    if !addressed {
        install_predecessor(&connection, 58).unwrap();
    }
    let payload = [7_u8; 160];
    {
        let transaction = schema::begin(&connection).unwrap();
        let mut versions = transaction.prepare("INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, x'0000000000000001', ?2)").unwrap();
        let mut heads = transaction
            .prepare("INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, x'0000000000000001')")
            .unwrap();
        for family in 0..FAMILIES {
            for id in 0..EXISTING {
                let key = key(family, id);
                versions
                    .execute(params![key.as_slice(), payload.as_slice()])
                    .unwrap();
                heads.execute([key.as_slice()]).unwrap();
            }
        }
        drop(versions);
        drop(heads);
        transaction.commit().unwrap();
    }
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    for commit in 0..COMMITS {
        let transaction = schema::begin(&connection).unwrap();
        for family in 0..FAMILIES {
            write::stage_record(
                &transaction,
                &key(family, EXISTING + commit),
                Some(&payload),
                CommitSequence::from_u64(commit + 2),
                &control,
            )
            .unwrap();
        }
        transaction.commit().unwrap();
    }
    let bytes = std::fs::metadata(path.with_extension("db-wal"))
        .unwrap()
        .len();
    assert_eq!((bytes - 32) % (4096 + 24), 0);
    let count: i64 = connection
        .query_row("SELECT count(*) FROM _uqa_mvcc_versions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count,
        i64::try_from(u64::from(FAMILIES) * (EXISTING + COMMITS)).unwrap()
    );
    (bytes - 32) / (4096 + 24)
}

#[test]
fn append_addressed_versions_reduce_wal_frames() {
    // Both layouts execute the same provider staging function and FULL commits.
    // Count durable pages, not elapsed time or an uncontrolled machine report.
    let predecessor = frames(false);
    let addressed = frames(true);
    assert!(
        addressed * 5 <= predecessor * 4,
        "addressed={addressed}, predecessor={predecessor}"
    );
    eprintln!("version WAL frames: {predecessor} -> {addressed}");
}
