//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sqlite_field_existence_does_not_hydrate_large_blobs_in_any_file_mode() {
    for provider in [Provider::Legacy, Provider::Native] {
        for mode in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("field-presence.sqlite");
            let connection = match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "presence-test"),
                2 => ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default()),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "presence-test",
                    SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap();
            let mut source = store(&connection, Provider::Legacy);
            source
                .put(
                    1,
                    [("blob".into(), Value::Bytes(vec![7; 512 << 10]))].into(),
                )
                .unwrap();
            // The public sparse writer normalizes NULL to absence. Seed an actual historical JSON null before binding the native versioned baseline.
            connection.with(|connection| {
                connection.execute("UPDATE _documents SET body = json_set(body, '$.null', json('null')) WHERE table_name = 'docs' AND doc_id = 1", [])?;
                Ok(())
            }).unwrap();
            if matches!(provider, Provider::Native) {
                connection
                    .bind_native_records(VersionedSessionOptions::default())
                    .unwrap();
                source = Box::new(SQLiteDocumentStore::new(connection.clone(), "docs"));
            }
            assert_eq!(source.get_field(1, "null").unwrap(), Some(Value::Null));
            let snapshot = matches!(provider, Provider::Native).then(|| source.snapshot().unwrap());
            let control = StorageReadControl::with_limit(8192);
            let page = read_field_presence(
                source.as_ref(),
                &[1, 99, 1],
                &["blob", "null", "absent"],
                &control,
            )
            .unwrap();
            assert_eq!(
                &*page,
                &[true, true, false, false, false, false, true, true, false]
            );
            assert!(control.memory().used() < 8192);
            drop(page);
            assert_eq!(control.memory().used(), 0);
            assert!(matches!(
                read_stored_documents(source.as_ref(), &[1], &control),
                Err(StorageBackendError::Memory(_))
            ));
            assert_eq!(control.memory().used(), 0);
            source.clear().unwrap();
            if let Some(snapshot) = snapshot {
                drop(source);
                drop(connection);
                let page =
                    read_field_presence(snapshot.as_ref(), &[1], &["blob", "null"], &control)
                        .unwrap();
                assert_eq!(&*page, &[true, true]);
                drop(page);
                let full = control.memory().reserve(control.memory().limit()).unwrap();
                assert!(matches!(
                    read_field_presence(snapshot.as_ref(), &[1], &["blob"], &control),
                    Err(StorageBackendError::Memory(_))
                ));
                drop(full);
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}
