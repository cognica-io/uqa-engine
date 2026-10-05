//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ManagedConnection, SQLiteCompressionOptions};

#[test]
fn encrypted_container_from_the_previous_cipher_version_reopens_and_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-encrypted.uqa");
    std::fs::write(
        &path,
        include_bytes!("fixtures/chacha20poly1305-0.10-zstd.uqa"),
    )
    .unwrap();
    let open = || {
        ManagedConnection::open_compressed_encrypted(
            &path,
            "legacy-fixture-public-key",
            SQLiteCompressionOptions::zstd(),
        )
        .unwrap()
    };
    let connection = open();
    connection
        .with(|raw| {
            let saved: String =
                raw.query_row("SELECT value FROM saved WHERE id = 7", [], |row| row.get(0))?;
            assert_eq!(saved, "legacy encrypted container");
            raw.execute("UPDATE saved SET value = 'updated' WHERE id = 7", [])?;
            Ok(())
        })
        .unwrap();
    drop(connection);
    open()
        .with(|raw| {
            let saved: String =
                raw.query_row("SELECT value FROM saved WHERE id = 7", [], |row| row.get(0))?;
            assert_eq!(saved, "updated");
            Ok(())
        })
        .unwrap();
}
