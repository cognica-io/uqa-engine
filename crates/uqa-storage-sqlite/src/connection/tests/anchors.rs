//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case(crate::compressed_vfs::SQLiteCompressionCodec::LZ4)]
#[case(crate::compressed_vfs::SQLiteCompressionCodec::Zstd)]
fn compressed_encrypted_anchor_allows_pool_growth_after_a_committed_write(
    #[case] codec: crate::compressed_vfs::SQLiteCompressionCodec,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anchored-pool.uqac.sqlite3");
    let options = SQLiteCompressionOptions {
        codec,
        ..SQLiteCompressionOptions::default()
    };
    let key = "trusted pool anchor key";
    {
        let connection = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        connection
            .with(|sqlite| {
                sqlite.execute_batch("CREATE TABLE t (value INTEGER); INSERT INTO t VALUES (1)")?;
                Ok(())
            })
            .unwrap();
    }
    let anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    let connection =
        ManagedConnection::open_compressed_encrypted_with_anchor(&path, key, options, anchor)
            .unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute("INSERT INTO t VALUES (2)", [])?;
            Ok(())
        })
        .unwrap();
    let retained = connection.lease_connection().unwrap();
    let additional = connection.lease_connection().unwrap();
    let total: i64 = additional
        .query_row("SELECT sum(value) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 3);
    // Both physical connections remain live across a second commit and a further pool expansion.
    retained.execute("INSERT INTO t VALUES (4)", []).unwrap();
    let observed: i64 = additional
        .query_row("SELECT sum(value) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(observed, 7);
    let third = connection.lease_connection().unwrap();
    let total: i64 = third
        .query_row("SELECT sum(value) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 7);
    drop((third, additional, retained));
    drop(connection);
    assert!(
        ManagedConnection::open_compressed_encrypted_with_anchor(&path, key, options, anchor)
            .is_err()
    );
}

#[test]
fn compressed_encrypted_anchor_rejects_whole_file_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anchored.uqac.sqlite3");
    let old_snapshot = dir.path().join("anchored-old.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let key = "trusted anchor key";

    {
        let connection = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        connection
            .with(|sqlite| {
                sqlite.execute_batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);",
                )?;
                Ok(())
            })
            .unwrap();
    }
    let old_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    std::fs::copy(&path, &old_snapshot).unwrap();

    {
        let connection = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        connection
            .with(|sqlite| {
                sqlite.execute("INSERT INTO t VALUES (2)", [])?;
                Ok(())
            })
            .unwrap();
    }
    let current_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    assert_eq!(current_anchor.database_id, old_anchor.database_id);
    assert!(current_anchor.generation > old_anchor.generation);
    assert_ne!(current_anchor.state_tag, old_anchor.state_tag);
    {
        let anchored = ManagedConnection::open_compressed_encrypted_with_anchor(
            &path,
            key,
            options,
            current_anchor,
        )
        .unwrap();
        anchored
            .with(|sqlite| {
                sqlite.execute("INSERT INTO t VALUES (3)", [])?;
                Ok(())
            })
            .unwrap();
    }
    let Err(error) = ManagedConnection::open_compressed_encrypted(&path, key, options) else {
        panic!("stale registered anchor unexpectedly accepted a newer state");
    };
    assert_ne!(error.to_string(), "");
    let advanced_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    assert!(advanced_anchor.generation > current_anchor.generation);
    ManagedConnection::open_compressed_encrypted_with_anchor(&path, key, options, advanced_anchor)
        .unwrap();

    assert!(ManagedConnection::open_compressed_encrypted_with_anchor(
        &path,
        key,
        options,
        current_anchor,
    )
    .is_err());

    std::fs::copy(&old_snapshot, &path).unwrap();
    let Err(error) = ManagedConnection::open_compressed_encrypted_with_anchor(
        &path,
        key,
        options,
        advanced_anchor,
    ) else {
        panic!("rolled-back container unexpectedly satisfied its trusted anchor");
    };
    assert!(error
        .to_string()
        .contains("does not match trusted generation"));

    let Err(error) = ManagedConnection::open_compressed_encrypted(&path, key, options) else {
        panic!("unanchored re-registration weakened an existing trusted anchor");
    };
    assert_ne!(error.to_string(), "");
}
