//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::{Connection, StatementStatus};
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{ConnectionFunctions, CONNECTIONS, TOKEN_FUNCTION};
use crate::mvcc::WritePermit;

fn permit_value(connection: &Connection) -> i64 {
    connection
        .prepare_cached("SELECT __uqa_mvcc_write_permit()")
        .unwrap()
        .query_row([], |row| row.get(0))
        .unwrap()
}

#[test]
fn admissions_and_captures_keep_prepared_statements_compiled() {
    let connection = Connection::open_in_memory().unwrap();
    drop(WritePermit::acquire(&connection).unwrap());
    assert_eq!(permit_value(&connection), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    for _ in 0..8 {
        let permit = WritePermit::acquire(&connection).unwrap();
        assert_eq!(permit_value(&connection), 1);
        drop(permit);
        assert_eq!(permit_value(&connection), 0);
        let functions = ConnectionFunctions::of(&connection).unwrap();
        functions.begin_capture(&control).unwrap();
        assert!(functions.end_capture().is_none());
    }
    let statement = connection
        .prepare_cached("SELECT __uqa_mvcc_write_permit()")
        .unwrap();
    assert_eq!(statement.get_status(StatementStatus::RePrepare), 0);
}

#[test]
fn native_keys_are_captured_only_inside_a_capture() {
    let connection = Connection::open_in_memory().unwrap();
    let functions = ConnectionFunctions::of(&connection).unwrap();
    let call = || {
        connection.query_row("SELECT __uqa_mvcc_native_key(65535)", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
    };
    assert!(call().is_err());
    assert!(functions.take_capture_error().is_none());
    let control = StorageReadControl::with_limit(1 << 20);
    functions.begin_capture(&control).unwrap();
    assert!(
        functions.begin_capture(&control).is_err(),
        "captures do not nest"
    );
    assert!(call().is_err());
    assert!(matches!(
        functions.take_capture_error(),
        Some(VersionError::InvalidEncoding(message)) if message.contains("unknown captured native family")
    ));
    assert!(call().is_err());
    assert!(functions.end_capture().is_some());
    assert!(call().is_err());
    assert!(functions.end_capture().is_none());
}

#[test]
fn closing_a_connection_releases_its_function_state() {
    let connection = Connection::open_in_memory().unwrap();
    let first = ConnectionFunctions::of(&connection).unwrap();
    let second = ConnectionFunctions::of(&connection).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    let token: i64 = connection
        .query_row(&format!("SELECT {TOKEN_FUNCTION}()"), [], |row| row.get(0))
        .unwrap();
    let token = u64::try_from(token).unwrap();
    assert!(CONNECTIONS.lock().contains_key(&token));
    drop(connection);
    assert!(!CONNECTIONS.lock().contains_key(&token));
    let other = Connection::open_in_memory().unwrap();
    assert!(!std::sync::Arc::ptr_eq(
        &first,
        &ConnectionFunctions::of(&other).unwrap()
    ));
}
