//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn copy_delete_publication_does_not_retain_complete_prepared_documents() {
    struct CheckedOutput {
        column: usize,
        rows: usize,
    }
    impl std::io::Write for CheckedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                if *byte == b'\n' {
                    assert_eq!(self.column, 32768);
                    self.column = 0;
                    self.rows += 1;
                } else {
                    assert_eq!(*byte, b'x');
                    self.column += 1;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let connection =
        uqa_storage_sqlite::ManagedConnection::open(&directory.path().join("delete.db")).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions {
            // Use the existing large-transaction fixture allowance; query workspace remains 64KiB.
            retained_bytes: 4 * 1024 * 1024,
        })
        .unwrap();
    let engine = Engine::from_persistent_provider(std::sync::Arc::new(
        uqa_storage_sqlite::SQLiteStorageProvider::new(connection),
    ))
    .unwrap();
    exec(
        &engine,
        "CREATE TABLE delete_output(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO delete_output SELECT n, repeat('x',32768) FROM generate_series(1,512) AS s(n)",
    );
    engine.set_query_memory_limit(Some(64 * 1024));
    let mut output = CheckedOutput { column: 0, rows: 0 };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            engine
                .copy_to(
                    "COPY (DELETE FROM delete_output RETURNING body) TO STDOUT",
                    &mut output
                )
                .unwrap(),
            512
        );
    });
    // Includes the 4MiB session allowance plus bounded query/spill overhead; excludes a complete 16MiB action queue.
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!((output.column, output.rows), (0, 512));
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*) FROM delete_output) TO STDOUT"
        ),
        "0\n"
    );
}
