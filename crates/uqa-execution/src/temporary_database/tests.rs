//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[cfg(not(target_os = "emscripten"))]
#[test]
fn private_index_values_and_rollback_journal_are_encrypted_without_database_credentials() {
    const SECRET: &str = "private-execution-index-secret-repeated-marker";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.db");
    let connection = open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE values_for_test (value TEXT)")
        .unwrap();
    connection
        .execute("INSERT INTO values_for_test VALUES (?1)", [SECRET])
        .unwrap();
    connection
        .execute_batch("BEGIN; UPDATE values_for_test SET value = value || value")
        .unwrap();
    assert!(path.with_extension("db-journal").exists());
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes
            .windows(SECRET.len())
            .any(|bytes| bytes == SECRET.as_bytes()));
    }
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        connection
            .query_row("SELECT value FROM values_for_test", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        SECRET
    );
    let plain = rusqlite::Connection::open(&path).unwrap();
    assert!(plain
        .execute_batch("SELECT * FROM values_for_test")
        .is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn failed_temporary_open_does_not_create_an_index_or_retain_its_directory() {
    let path = {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let obstruction = path.join("not-a-directory");
        std::fs::write(&obstruction, b"unchanged").unwrap();
        assert!(open(&obstruction.join("index.db")).is_err());
        assert_eq!(std::fs::read(obstruction).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        path
    };
    assert!(!path.exists());
}
