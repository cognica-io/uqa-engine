//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded evaluated mutation publication through the public COPY API.

use super::{copy_to_string, exec, Engine};

struct CheckedOutput {
    width: usize,
    column: usize,
    rows: usize,
}

impl std::io::Write for CheckedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if *byte == b'\n' {
                assert_eq!(self.column, self.width);
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

fn allocation_engine(path: &std::path::Path) -> Engine {
    let connection = uqa_storage_sqlite::ManagedConnection::open(path).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions {
            retained_bytes: 4 * 1024 * 1024,
        })
        .unwrap();
    Engine::from_persistent_provider(std::sync::Arc::new(
        uqa_storage_sqlite::SQLiteStorageProvider::new(connection),
    ))
    .unwrap()
}

#[test]
fn copy_delete_publication_does_not_retain_complete_prepared_documents() {
    let directory = tempfile::tempdir().unwrap();
    let engine = allocation_engine(&directory.path().join("delete.db"));
    exec(
        &engine,
        "CREATE TABLE delete_output(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO delete_output SELECT n, repeat('x',32768) FROM generate_series(1,512) AS s(n)",
    );
    engine.set_query_memory_limit(Some(64 * 1024));
    let mut output = CheckedOutput {
        width: 32768,
        column: 0,
        rows: 0,
    };
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

#[test]
fn copy_update_publication_does_not_retain_complete_prepared_documents() {
    let directory = tempfile::tempdir().unwrap();
    let engine = allocation_engine(&directory.path().join("update.db"));
    exec(
        &engine,
        "CREATE TABLE update_output(id INTEGER PRIMARY KEY, body TEXT, changed INTEGER);
        INSERT INTO update_output SELECT n, repeat('x',32768), 0 FROM generate_series(1,512) AS s(n)",
    );
    engine.set_query_memory_limit(Some(64 * 1024));
    let mut output = CheckedOutput {
        width: 32768,
        column: 0,
        rows: 0,
    };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            engine
                .copy_to(
                    "COPY (UPDATE update_output SET changed = 1 RETURNING body) TO STDOUT",
                    &mut output,
                )
                .unwrap(),
            512
        );
    });
    // Includes the 4MiB session allowance plus bounded query/spill overhead; excludes the complete old/new action images.
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!((output.column, output.rows), (0, 512));
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*), min(changed), max(changed) FROM update_output) TO STDOUT"
        ),
        "512\t1\t1\n"
    );
}

#[rstest::rstest]
#[case::values(false)]
#[case::select(true)]
fn copy_trigger_view_insert_keeps_bounded_input_and_proposed_rows(#[case] select: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = allocation_engine(&directory.path().join("view-insert.db"));
    exec(
        &engine,
        "CREATE TABLE view_input_base(id INTEGER, body TEXT);
        CREATE VIEW view_input_target AS SELECT * FROM view_input_base;
        CREATE FUNCTION retain_view_row() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN RETURN NEW; END $$;
        CREATE TRIGGER retain_view_row INSTEAD OF INSERT ON view_input_target
        FOR EACH ROW EXECUTE FUNCTION retain_view_row()",
    );
    engine.set_query_memory_limit(Some(512 * 1024));
    let source = if select {
        "SELECT n, repeat('x',131072) FROM generate_series(1,128) AS s(n)".to_owned()
    } else {
        let values = (1..=128)
            .map(|id| format!("({id},repeat('x',131072))"))
            .collect::<Vec<_>>()
            .join(",");
        format!("VALUES {values}")
    };
    let sql = format!("COPY (INSERT INTO view_input_target {source} RETURNING body) TO STDOUT");
    let mut output = CheckedOutput {
        width: 131_072,
        column: 0,
        rows: 0,
    };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(engine.copy_to(&sql, &mut output).unwrap(), 128);
    });
    // A complete 16MiB source or proposed-row set cannot fit in this fixed allowance.
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!((output.column, output.rows), (0, 128));
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*) FROM view_input_base) TO STDOUT"
        ),
        "0\n"
    );
}
