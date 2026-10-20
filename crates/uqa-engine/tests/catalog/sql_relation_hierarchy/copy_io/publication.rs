//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded evaluated mutation publication through the public COPY API.

use super::{copy_engine, copy_to_string, exec, Engine};

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

#[test]
fn copy_values_insert_publication_does_not_retain_complete_prepared_documents() {
    let directory = tempfile::tempdir().unwrap();
    let engine = allocation_engine(&directory.path().join("insert.db"));
    exec(
        &engine,
        "CREATE TABLE insert_output(id INTEGER PRIMARY KEY, body TEXT)",
    );
    // Each scalar result is 128KiB; the source contains compact expressions, not 16MiB of literal payloads.
    engine.set_query_memory_limit(Some(256 * 1024));
    let values = (1..=128)
        .map(|id| format!("({id},repeat('x',131072))"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("COPY (INSERT INTO insert_output VALUES {values} RETURNING body) TO STDOUT");
    let mut output = CheckedOutput {
        width: 131_072,
        column: 0,
        rows: 0,
    };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(engine.copy_to(&sql, &mut output).unwrap(), 128);
    });
    // Same 4MiB session allowance plus 4MiB bounded query/spill overhead; a complete 16MiB prepared queue cannot fit.
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!((output.column, output.rows), (0, 128));
    assert_eq!(copy_to_string(&engine, "COPY (SELECT count(*), min(length(body)), max(length(body)) FROM insert_output) TO STDOUT"), "128\t131072\t131072\n");
}

#[rstest::rstest]
#[case::values(false)]
#[case::select(true)]
fn copy_trigger_view_insert_keeps_bounded_input_and_proposed_rows(#[case] select: bool) {
    assert_bounded_trigger_view_insert(select, false);
}

#[rstest::rstest]
#[case::values(false)]
#[case::select(true)]
fn copy_rule_trigger_view_insert_keeps_bounded_input_and_proposed_rows(#[case] select: bool) {
    assert_bounded_trigger_view_insert(select, true);
}

fn assert_bounded_trigger_view_insert(select: bool, rule: bool) {
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
    if rule {
        exec(
            &engine,
            "CREATE TABLE view_input_audit(id INTEGER);
            CREATE RULE audit_view_input AS ON INSERT TO view_input_target
            DO ALSO INSERT INTO view_input_audit VALUES(NEW.id)",
        );
    }
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
    if rule {
        assert_eq!(
            copy_to_string(
                &engine,
                "COPY (SELECT count(*),sum(id) FROM view_input_audit) TO STDOUT"
            ),
            "128\t8256\n"
        );
    }
}

#[test]
fn rule_values_insert_does_not_retain_complete_candidate_images() {
    let values = (1..=128)
        .map(|id| format!("({id},repeat('x',131072))"))
        .collect::<Vec<_>>()
        .join(",");
    assert_rule_insert_memory_bound(&format!("VALUES {values}"));
}

#[test]
fn rule_select_insert_does_not_retain_complete_query_input() {
    assert_rule_insert_memory_bound(
        "SELECT n,repeat('x',131072) FROM generate_series(1,128) AS s(n)",
    );
}

fn assert_rule_insert_memory_bound(source: &str) {
    let directory = tempfile::tempdir().unwrap();
    let engine = allocation_engine(&directory.path().join("rule-insert.db"));
    exec(
        &engine,
        "CREATE TABLE rule_insert_output(id INTEGER PRIMARY KEY, body TEXT);
         CREATE TABLE rule_insert_log(id INTEGER);
         CREATE RULE capture AS ON INSERT TO rule_insert_output
         DO ALSO INSERT INTO rule_insert_log VALUES (NEW.id)",
    );
    engine.set_query_memory_limit(Some(256 * 1024));
    let sql = format!("INSERT INTO rule_insert_output {source}");
    let allocated = allocation_counter::measure(|| exec(&engine, &sql));
    // The 4MiB session allowance plus bounded spill overhead cannot retain all 16MiB of candidates.
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*), min(length(body)), max(length(body)) FROM rule_insert_output) TO STDOUT"
        ),
        "128\t131072\t131072\n"
    );
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*), sum(id) FROM rule_insert_log) TO STDOUT"
        ),
        "128\t8256\n"
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn rule_select_input_preserves_positions_and_rolls_back_source_failure(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rule-source.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE positions_probe(id INTEGER PRIMARY KEY, value INTEGER);
        CREATE TABLE positions_probe_log(id INTEGER,value INTEGER);
        CREATE RULE capture AS ON INSERT TO positions_probe
        DO ALSO INSERT INTO positions_probe_log VALUES (NEW.id,NEW.value)",
    );
    engine.set_query_memory_limit(Some(1024));
    exec(
        &engine,
        "INSERT INTO positions_probe
        SELECT n AS same,n+10 AS same FROM generate_series(1,3) AS s(n)",
    );
    for table in ["positions_probe", "positions_probe_log"] {
        assert_eq!(
            copy_to_string(
                &engine,
                &format!("COPY (SELECT id,value FROM {table} ORDER BY id) TO STDOUT")
            ),
            "1\t11\n2\t12\n3\t13\n"
        );
    }
    exec(&engine, "BEGIN; SAVEPOINT input_failure");
    let error = engine
        .sql(
            "INSERT INTO positions_probe
        SELECT n+10,10/(3-n) FROM generate_series(1,3) AS s(n)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    exec(&engine, "ROLLBACK TO input_failure; COMMIT");
    for table in ["positions_probe", "positions_probe_log"] {
        assert_eq!(
            copy_to_string(
                &engine,
                &format!("COPY (SELECT count(*),sum(value) FROM {table}) TO STDOUT")
            ),
            "3\t36\n"
        );
    }
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        for table in ["positions_probe", "positions_probe_log"] {
            assert_eq!(
                copy_to_string(
                    &reopened,
                    &format!("COPY (SELECT id,value FROM {table} ORDER BY id) TO STDOUT")
                ),
                "1\t11\n2\t12\n3\t13\n"
            );
        }
    }
}
