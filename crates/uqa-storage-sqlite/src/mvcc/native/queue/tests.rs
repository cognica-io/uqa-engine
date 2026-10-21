//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn work_queue_seeks_keys_without_rescanning_a_constant_family() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE _uqa_mvcc_native_expected (family INTEGER, physical_key BLOB, PRIMARY KEY (family, physical_key))").unwrap();
    let families = [NativeRecordFamily::Documents, NativeRecordFamily::Vectors];
    for family in families {
        for number in 0_u64..1024 {
            connection
                .execute(
                    "INSERT INTO _uqa_mvcc_native_expected VALUES (?1, ?2)",
                    params![family.id(), number.to_be_bytes().as_slice()],
                )
                .unwrap();
        }
    }
    let instructions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&instructions);
    connection
        .progress_handler(
            1,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    let mut actual = Vec::new();
    let condition = format!("family = {}", families[0].id());
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        &condition,
        &control,
        |family, key| {
            actual.push((family.id(), u64::from_be_bytes(key.try_into().unwrap())));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        actual,
        (0..1024)
            .map(|number| (families[0].id(), number))
            .collect::<Vec<_>>()
    );
    assert!(
        instructions.load(Ordering::Relaxed) < 150_000,
        "queue used {} instructions",
        instructions.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), 0);
    actual.clear();
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |family, key| {
            actual.push((family.id(), u64::from_be_bytes(key.try_into().unwrap())));
            Ok(())
        },
    )
    .unwrap();
    let mut expected = families
        .into_iter()
        .flat_map(|family| (0..1024).map(move |number| (family.id(), number)))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert_eq!(control.memory().used(), 0);
}

fn queue(connection: &Connection) {
    connection
        .execute_batch("CREATE TABLE _uqa_mvcc_native_expected (family INTEGER, physical_key BLOB, new_value BLOB, PRIMARY KEY (family, physical_key)) WITHOUT ROWID")
        .unwrap();
}

/// A key of `length` bytes that sorts by `ordinal`.
fn key(ordinal: u8, length: usize) -> Vec<u8> {
    let mut key = vec![0_u8; length];
    key[0] = ordinal;
    key
}

#[test]
fn a_long_key_ends_its_page_and_is_read_once_its_length_is_reserved() {
    let connection = Connection::open_in_memory().unwrap();
    queue(&connection);
    let family = NativeRecordFamily::Documents.id();
    let lengths = [8, INLINE_KEY_BYTES, INLINE_KEY_BYTES + 1, 8, 5000, 3000, 8];
    for (ordinal, length) in lengths.into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_native_expected (family, physical_key) VALUES (?1, ?2)",
                params![family, key(ordinal as u8, length)],
            )
            .unwrap();
    }
    let control = StorageReadControl::with_limit(65536);
    let mut visited = Vec::new();
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |_, key| {
            visited.push((key[0], key.len()));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        visited,
        lengths
            .into_iter()
            .enumerate()
            .map(|(ordinal, length)| (ordinal as u8, length))
            .collect::<Vec<_>>()
    );
    assert_eq!(control.memory().used(), 0);

    // A key longer than the allowance left is refused before it is read, after the rows before it were visited.
    let control = StorageReadControl::with_limit(4096);
    visited.clear();
    let refused = visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |_, key| {
            visited.push((key[0], key.len()));
            Ok(())
        },
    );
    assert!(matches!(
        refused,
        Err(crate::mvcc::Error::Version(VersionError::Memory(_)))
    ));
    assert_eq!(visited.last(), Some(&(3, 8)));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_key_page_never_loads_payload_values() {
    let connection = Connection::open_in_memory().unwrap();
    queue(&connection);
    let family = NativeRecordFamily::Documents.id();
    for ordinal in 0..64 {
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_native_expected VALUES (?1, ?2, zeroblob(131072))",
                params![family, key(ordinal, 4)],
            )
            .unwrap();
    }
    let control = StorageReadControl::with_limit(4096);
    let mut visited = 0;
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |_, key| {
            assert_eq!(key[0], visited);
            visited += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(visited, 64);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_visitor_that_changes_another_table_sees_every_queued_row() {
    let connection = Connection::open_in_memory().unwrap();
    queue(&connection);
    connection
        .execute_batch("CREATE TABLE visited (physical_key BLOB PRIMARY KEY)")
        .unwrap();
    let family = NativeRecordFamily::Documents.id();
    for number in 0_u64..300 {
        connection
            .execute(
                "INSERT INTO _uqa_mvcc_native_expected (family, physical_key) VALUES (?1, ?2)",
                params![family, number.to_be_bytes().as_slice()],
            )
            .unwrap();
    }
    let control = StorageReadControl::with_limit(65536);
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |_, key| {
            connection.execute("INSERT INTO visited VALUES (?1)", [key])?;
            Ok(())
        },
    )
    .unwrap();
    let visited: i64 = connection
        .query_row("SELECT count(*) FROM visited", [], |row| row.get(0))
        .unwrap();
    assert_eq!(visited, 300);
}
