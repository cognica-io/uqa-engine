//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` text/CSV COPY over ordinary inheritance and partitions.

use super::{exec, Engine, Value};

fn copy_to_string(engine: &Engine, statement: &str) -> String {
    let mut output = Vec::new();
    engine.copy_to(statement, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

fn copy_engine(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[path = "copy_io/rules.rs"]
mod rules;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_spilled_returning_preserves_values_and_row_images(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("returning-spill.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE returning_spill (id INTEGER PRIMARY KEY, body TEXT)",
    );
    engine.set_query_memory_limit(Some(1024));
    let body = "x".repeat(4096);
    let inserted = copy_to_string(&engine, "COPY (INSERT INTO returning_spill SELECT n, repeat('x', 4096) FROM generate_series(1, 8) AS s(n) RETURNING id AS item, body AS item) TO STDOUT WITH (FORMAT csv, HEADER)");
    let mut expected = String::from("item,item\n");
    for id in 1..=8 {
        use std::fmt::Write;
        writeln!(expected, "{id},{body}").unwrap();
    }
    assert_eq!(inserted, expected);
    assert_eq!(copy_to_string(&engine, "COPY (WITH changed AS (UPDATE returning_spill SET body = body RETURNING id AS item, body AS item) SELECT * FROM changed ORDER BY 1) TO STDOUT WITH (FORMAT csv, HEADER)"), expected);
    let updated = copy_to_string(&engine, "COPY (UPDATE returning_spill SET body = body || '!' RETURNING old.body AS item, new.body AS item) TO STDOUT WITH (FORMAT csv)");
    assert_eq!(updated, format!("{body},{body}!\n").repeat(8));
    let from = copy_to_string(&engine, "COPY (UPDATE returning_spill AS t SET body = body || '?' FROM (VALUES (1)) AS s(id) WHERE t.id = s.id RETURNING old.body, new.body) TO STDOUT WITH (FORMAT csv)");
    assert_eq!(from, format!("{body}!,{body}!?\n"));
    let merged = copy_to_string(&engine, "COPY (MERGE INTO returning_spill AS t USING (VALUES (1), (9)) AS s(id) ON t.id = s.id WHEN MATCHED THEN UPDATE SET body = 'merged' WHEN NOT MATCHED THEN INSERT VALUES (s.id, repeat('y', 4096)) RETURNING merge_action(), old.body, new.body) TO STDOUT WITH (FORMAT csv)");
    let mut actual = merged.lines().collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(
        actual,
        [
            format!("INSERT,,{}", "y".repeat(4096)),
            format!("UPDATE,{body}!?,merged")
        ]
    );
    let deleted = copy_to_string(&engine, "COPY (DELETE FROM returning_spill AS t USING (VALUES (9)) AS s(id) WHERE t.id = s.id RETURNING old.body, new.body) TO STDOUT WITH (FORMAT csv)");
    assert_eq!(deleted, format!("{},\n", "y".repeat(4096)));
    assert_eq!(copy_to_string(&engine, "COPY (DELETE FROM returning_spill WHERE id < 0 RETURNING id AS item, body AS item) TO STDOUT WITH (FORMAT csv, HEADER)"), "item,item\n");
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        assert_eq!(
            reopened
                .sql("SELECT body FROM returning_spill WHERE id = 1", &[])
                .unwrap()
                .rows[0]["body"],
            Value::Str("merged".into())
        );
        assert_eq!(
            reopened
                .sql("SELECT * FROM returning_spill", &[])
                .unwrap()
                .rows
                .len(),
            8
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_rule_returning_keeps_source_context_and_rolls_back_failed_delivery(
    #[case] provider: usize,
) {
    struct FailedFlush;
    impl std::io::Write for FailedFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("rule output closed"))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rule-returning.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE rule_bounded_event(id INTEGER PRIMARY KEY, body TEXT);
        CREATE TABLE rule_bounded_target(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO rule_bounded_event VALUES (1, repeat('x', 4096)), (2, repeat('x', 4096));
        INSERT INTO rule_bounded_target VALUES (1, 'target-before'), (2, 'target-before');
        CREATE RULE redirect AS ON UPDATE TO rule_bounded_event DO INSTEAD
          UPDATE rule_bounded_target SET body = NEW.body || '-action'
          WHERE rule_bounded_target.id = OLD.id RETURNING rule_bounded_target.*",
    );
    engine.set_query_memory_limit(Some(1024));
    let copy = "COPY (UPDATE rule_bounded_event AS e SET body=e.body || '-new'
        FROM (VALUES (1, 'source-one'), (2, 'source-two')) AS s(id,label) WHERE e.id=s.id
        RETURNING e.id AS item, old.body AS item, new.body AS item, s.label AS item)
        TO STDOUT WITH (FORMAT csv, HEADER)";
    assert_eq!(
        engine
            .copy_to(copy, &mut FailedFlush)
            .unwrap_err()
            .sqlstate(),
        Some("58030")
    );
    let before = engine
        .sql("SELECT body FROM rule_bounded_target ORDER BY id", &[])
        .unwrap();
    assert_eq!(before.rows.len(), 2);
    assert!(before
        .rows
        .iter()
        .all(|row| row["body"] == Value::Str("target-before".into())));

    let output = copy_to_string(&engine, copy);
    let mut lines = output.lines();
    assert_eq!(lines.next(), Some("item,item,item,item"));
    let mut actual = lines.collect::<Vec<_>>();
    actual.sort_unstable();
    let body = format!("{}-new-action", "x".repeat(4096));
    assert_eq!(
        actual,
        [
            format!("1,target-before,{body},source-one"),
            format!("2,target-before,{body},source-two")
        ]
    );
    assert!(engine
        .sql("SELECT body FROM rule_bounded_event", &[])
        .unwrap()
        .rows
        .iter()
        .all(|row| row["body"] == Value::Str("x".repeat(4096))));
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        let rows = reopened
            .sql("SELECT body FROM rule_bounded_target ORDER BY id", &[])
            .unwrap();
        assert_eq!(rows.rows.len(), 2);
        assert!(rows
            .rows
            .iter()
            .all(|row| row["body"] == Value::Str(body.clone())));
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_output_failure_rolls_back_the_implicit_mutation(#[case] provider: usize) {
    struct Sink {
        mode: &'static str,
        token: uqa_core::CancellationToken,
        writes: usize,
        flushes: usize,
        bytes: Vec<u8>,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            if self.mode == "write" || (self.mode == "partial" && self.writes > 1) {
                return Err(std::io::Error::other("closed COPY sink"));
            }
            let count = if self.mode == "partial" {
                1
            } else {
                bytes.len()
            };
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            match self.mode {
                "flush" => Err(std::io::Error::other("failed COPY flush")),
                "cancel" => {
                    self.token.cancel();
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy-output-failure.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE copy_atomic (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "INSERT INTO copy_atomic VALUES (1, 10)");
    let original = engine
        .sql("SELECT * FROM copy_atomic ORDER BY id", &[])
        .unwrap()
        .rows;
    for statement in [
        "INSERT INTO copy_atomic VALUES (2, 20) RETURNING id, value",
        "UPDATE copy_atomic SET value = 99 RETURNING id, value",
        "DELETE FROM copy_atomic RETURNING id, value",
        "WITH changed AS (INSERT INTO copy_atomic VALUES (2, 20) RETURNING id, value) SELECT * FROM changed",
    ] {
        for mode in ["write", "partial", "flush", "cancel"] {
            let mut sink = Sink {
                mode,
                token: engine.cancellation_token(),
                writes: 0,
                flushes: 0,
                bytes: Vec::new(),
            };
            let error = engine.copy_to(&format!("COPY ({statement}) TO STDOUT"), &mut sink).unwrap_err();
            assert_eq!(error.sqlstate(), Some(if mode == "cancel" { "57014" } else { "58030" }));
            assert_eq!(sink.flushes, usize::from(matches!(mode, "flush" | "cancel")));
            if mode == "partial" {
                assert_eq!(sink.bytes.len(), 1);
            }
            engine.reset_cancellation();
            assert_eq!(engine.sql("SELECT * FROM copy_atomic ORDER BY id", &[]).unwrap().rows, original, "{mode}: {statement}");
        }
    }
    drop(engine);
    let engine = if provider == 0 {
        let engine = copy_engine(provider, &path);
        exec(
            &engine,
            "CREATE TABLE copy_atomic (id INTEGER PRIMARY KEY, value INTEGER)",
        );
        exec(&engine, "INSERT INTO copy_atomic VALUES (1, 10)");
        engine
    } else {
        let engine = copy_engine(provider, &path);
        assert_eq!(
            engine
                .sql("SELECT * FROM copy_atomic ORDER BY id", &[])
                .unwrap()
                .rows,
            original
        );
        engine
    };
    let mut sink = Sink {
        mode: "success",
        token: engine.cancellation_token(),
        writes: 0,
        flushes: 0,
        bytes: Vec::new(),
    };
    assert_eq!(
        engine
            .copy_to(
                "COPY (INSERT INTO copy_atomic VALUES (2, 20) RETURNING id, value) TO STDOUT",
                &mut sink
            )
            .unwrap(),
        1
    );
    assert_eq!(sink.bytes, b"2\t20\n");
    assert_eq!(sink.flushes, 1);
    assert_eq!(
        engine
            .sql("SELECT * FROM copy_atomic", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    drop(engine);
    if provider != 0 {
        assert_eq!(
            copy_engine(provider, &path)
                .sql("SELECT * FROM copy_atomic", &[])
                .unwrap()
                .rows
                .len(),
            2
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_spooled_query_preserves_values_and_statement_snapshot(#[case] provider: usize) {
    struct UpdatingSink<'a> {
        writer: Option<&'a Engine>,
        bytes: Vec<u8>,
        updated: bool,
        flushes: usize,
    }
    impl std::io::Write for UpdatingSink<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.updated {
                if let Some(writer) = self.writer {
                    exec(writer, "UPDATE copy_snapshot SET value = 'after'");
                }
                self.updated = true;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let engine = copy_engine(provider, &directory.path().join("copy-snapshot.db"));
    exec(
        &engine,
        "CREATE TABLE copy_snapshot (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(&engine, "INSERT INTO copy_snapshot VALUES (1, 'before')");
    engine.set_query_memory_limit(Some(1024));
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut sink = UpdatingSink {
        writer: writer.as_ref(),
        bytes: Vec::new(),
        updated: false,
        flushes: 0,
    };
    assert_eq!(engine.copy_to("COPY (SELECT value, n, n + 1 AS n FROM copy_snapshot CROSS JOIN generate_series(1, 2048) AS series(n) ORDER BY 2) TO STDOUT WITH (FORMAT csv, HEADER)", &mut sink).unwrap(), 2048);
    let mut expected = String::from("value,n,n\n");
    for n in 1..=2048 {
        use std::fmt::Write;
        writeln!(expected, "before,{n},{}", n + 1).unwrap();
    }
    assert_eq!(sink.bytes, expected.as_bytes());
    assert_eq!(sink.flushes, 1);
    assert_eq!(
        engine
            .sql("SELECT value FROM copy_snapshot", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str(if provider == 0 { "before" } else { "after" }.into())
    );
    // Reuse cached plans and retain output metadata even when the spool has no rows.
    for _ in 0..2 {
        assert_eq!(copy_to_string(&engine, "COPY (SELECT id, value FROM copy_snapshot WHERE id < 0) TO STDOUT WITH (FORMAT csv, HEADER)"), "id,value\n");
        assert_eq!(
            copy_to_string(
                &engine,
                "COPY (SELECT 1 AS x UNION ALL SELECT 2 ORDER BY x DESC) TO STDOUT"
            ),
            "2\n1\n"
        );
    }
}

#[test]
fn copy_output_preserves_session_style_duplicate_labels_and_mutation_returning() {
    let engine = Engine::new();
    exec(&engine, "SET intervalstyle = 'iso_8601'");
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT INTERVAL '1 day 2 hours', 1 AS x, 2 AS x) TO STDOUT"
        ),
        "P1DT2H\t1\t2\n"
    );
    assert_eq!(copy_to_string(&engine, "COPY (SELECT set_config('IntervalStyle', 'postgres_verbose', false), INTERVAL '1 day 2 hours') TO STDOUT"), "postgres_verbose\t@ 1 day 2 hours\n");
    exec(
        &engine,
        "CREATE TABLE copy_returning (id INTEGER PRIMARY KEY, value TEXT)",
    );
    assert_eq!(copy_to_string(&engine, "COPY (INSERT INTO copy_returning VALUES (1, 'before') RETURNING id, value) TO STDOUT WITH (FORMAT csv)"), "1,before\n");
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (UPDATE copy_returning SET value = 'after' RETURNING id, value) TO STDOUT"
        ),
        "1\tafter\n"
    );
    assert_eq!(copy_to_string(&engine, "COPY (WITH removed AS (DELETE FROM copy_returning RETURNING id, value) SELECT * FROM removed) TO STDOUT"), "1\tafter\n");
    assert!(engine
        .sql("SELECT * FROM copy_returning", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn copy_output_sink_cancellation_and_io_error_abort_explicit_transaction() {
    struct Sink {
        token: uqa_core::CancellationToken,
        cancel: bool,
        calls: usize,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.cancel {
                self.token.cancel();
                Ok(bytes.len())
            } else {
                Err(std::io::Error::other("closed export sink"))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE copy_failure (id INTEGER)");
    for cancel in [false, true] {
        exec(&engine, "BEGIN");
        exec(&engine, "INSERT INTO copy_failure VALUES (1)");
        let mut sink = Sink {
            token: engine.cancellation_token(),
            cancel,
            calls: 0,
        };
        let error = engine
            .copy_to("COPY (SELECT repeat('x', 20000)) TO STDOUT", &mut sink)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancel { "57014" } else { "58030" })
        );
        assert_eq!(sink.calls, 1);
        engine.reset_cancellation();
        assert_eq!(
            engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
            Some("25P02")
        );
        exec(&engine, "ROLLBACK");
        assert!(engine
            .sql("SELECT * FROM copy_failure", &[])
            .unwrap()
            .rows
            .is_empty());
    }
}

#[test]
fn ordinary_inheritance_copy_uses_named_storage_and_query_scan_semantics() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_parent (id INTEGER, payload TEXT DEFAULT 'parent-default', doubled INTEGER GENERATED ALWAYS AS (id * 2) STORED, CHECK (id > 0))",
    );
    exec(
        &engine,
        "CREATE TABLE copy_child (extra TEXT DEFAULT 'child-default') INHERITS (copy_parent)",
    );

    assert_eq!(
        engine
            .copy_from("COPY copy_parent (id) FROM STDIN", b"1\n".as_slice())
            .unwrap(),
        1
    );
    assert_eq!(
        engine
            .copy_from(
                "COPY copy_child (id, extra) FROM STDIN",
                b"2\tchild\n".as_slice(),
            )
            .unwrap(),
        1
    );

    assert_eq!(
        copy_to_string(&engine, "COPY copy_parent TO STDOUT"),
        "1\tparent-default\n"
    );
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT id, payload, doubled FROM copy_parent ORDER BY id) TO STDOUT",
        ),
        "1\tparent-default\t2\n2\tparent-default\t4\n"
    );
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT id, payload, doubled FROM ONLY copy_parent ORDER BY id) TO STDOUT",
        ),
        "1\tparent-default\t2\n"
    );
    let only_syntax = engine
        .copy_to("COPY ONLY copy_parent TO STDOUT", Vec::new())
        .unwrap_err();
    assert_eq!(only_syntax.sqlstate(), Some("42601"));

    exec(&engine, "CREATE SCHEMA \"copy.schema\"");
    exec(
        &engine,
        "CREATE TABLE \"copy.schema\".\"Odd.Table\" (\"Odd Column\" INTEGER, payload TEXT DEFAULT 'quoted')",
    );
    assert_eq!(
        engine
            .copy_from(
                "COPY \"copy.schema\".\"Odd.Table\" (\"Odd Column\") FROM STDIN",
                b"7\n".as_slice(),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        copy_to_string(&engine, "COPY \"copy.schema\".\"Odd.Table\" TO STDOUT",),
        "7\tquoted\n"
    );
}

fn create_range_copy_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE copy_ranges (id BIGINT GENERATED BY DEFAULT AS IDENTITY, bucket INTEGER, payload TEXT DEFAULT 'range-default', generated_value TEXT GENERATED ALWAYS AS (payload || ':' || bucket::text) STORED, CHECK (bucket >= 0)) PARTITION BY RANGE (bucket)",
    );
    exec(
        engine,
        "CREATE TABLE copy_ranges_low PARTITION OF copy_ranges FOR VALUES FROM (0) TO (10)",
    );
    exec(
        engine,
        "CREATE TABLE copy_ranges_high PARTITION OF copy_ranges FOR VALUES FROM (10) TO (20)",
    );
    exec(
        engine,
        "CREATE TABLE copy_ranges_default PARTITION OF copy_ranges DEFAULT",
    );
}

fn assert_range_copy_rows(engine: &Engine) {
    let rows = engine
        .sql(
            "SELECT id, bucket, payload, generated_value FROM copy_ranges ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 3);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
    assert_eq!(rows.rows[0]["generated_value"], Value::Str("one:1".into()));
    assert_eq!(rows.rows[1]["id"], Value::Int(2));
    assert_eq!(rows.rows[2]["id"], Value::Int(3));
    for table in ["copy_ranges_low", "copy_ranges_high", "copy_ranges_default"] {
        assert_eq!(
            engine
                .sql(&format!("SELECT count(*) AS n FROM {table}"), &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(1)
        );
    }
}

fn assert_range_copy_output(engine: &Engine) {
    let missing = engine
        .copy_to("COPY copy_ranges(no_such_column) TO STDOUT", Vec::new())
        .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42703"));
    let generated = engine
        .copy_to("COPY copy_ranges(generated_value) TO STDOUT", Vec::new())
        .unwrap_err();
    assert_eq!(generated.sqlstate(), Some("42P10"));
    let partitioned_to = engine
        .copy_to("COPY copy_ranges TO STDOUT", Vec::new())
        .unwrap_err();
    assert_eq!(partitioned_to.sqlstate(), Some("42809"));
    assert_eq!(
        copy_to_string(engine, "COPY copy_ranges_low TO STDOUT"),
        "1\t1\tone\n"
    );
    assert_eq!(
        copy_to_string(
            engine,
            "COPY (SELECT id, bucket, payload, generated_value FROM copy_ranges ORDER BY id) TO STDOUT",
        ),
        "1\t1\tone\tone:1\n2\t11\ttwo\ttwo:11\n3\t21\tthree\tthree:21\n"
    );
    assert_eq!(
        copy_to_string(engine, "COPY (SELECT * FROM ONLY copy_ranges) TO STDOUT"),
        ""
    );
}

#[test]
fn range_copy_routes_defaults_generated_identity_and_rolls_back_failures() {
    let engine = Engine::new();
    create_range_copy_fixture(&engine);

    assert_eq!(
        engine
            .copy_from(
                "COPY copy_ranges (bucket, payload) FROM STDIN",
                b"1\tone\n11\ttwo\n21\tthree\n".as_slice(),
            )
            .unwrap(),
        3
    );
    assert_range_copy_rows(&engine);
    assert_range_copy_output(&engine);

    let generated = engine
        .copy_from(
            "COPY copy_ranges (bucket, generated_value) FROM STDIN",
            b"2\tmanual\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(generated.sqlstate(), Some("42P10"));

    let bound = engine
        .copy_from(
            "COPY copy_ranges_low (bucket, payload) FROM STDIN",
            b"2\twould-rollback\n12\tinvalid\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(bound.sqlstate(), Some("23514"));
    assert_eq!(
        engine
            .sql(
                "SELECT * FROM copy_ranges WHERE payload = 'would-rollback'",
                &[],
            )
            .unwrap()
            .rows
            .len(),
        0
    );

    let parse = engine
        .copy_from(
            "COPY copy_ranges (bucket, payload) FROM STDIN",
            b"3\twould-rollback\nnot-an-integer\tinvalid\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(parse.sqlstate(), Some("22P02"));
    assert_eq!(
        engine
            .sql(
                "SELECT * FROM copy_ranges WHERE payload = 'would-rollback'",
                &[],
            )
            .unwrap()
            .rows
            .len(),
        0
    );

    let extra = engine
        .copy_from(
            "COPY copy_ranges (bucket, payload) FROM STDIN",
            b"5\tok\textra\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(extra.sqlstate(), Some("22P04"));
    let missing = engine
        .copy_from(
            "COPY copy_ranges (bucket, payload) FROM STDIN",
            b"5\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("22P04"));
}

#[test]
fn nested_direct_leaf_copy_checks_every_ancestor_and_aborts_transactions_on_format_errors() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_copy_root (id INTEGER, region INTEGER, bucket INTEGER, payload TEXT) PARTITION BY RANGE (region)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_copy_local PARTITION OF nested_copy_root FOR VALUES FROM (0) TO (100) PARTITION BY LIST (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_copy_local_one PARTITION OF nested_copy_local FOR VALUES IN (1)",
    );
    engine
        .copy_from(
            "COPY nested_copy_root FROM STDIN",
            b"1\t10\t1\trouted\n".as_slice(),
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT payload FROM nested_copy_local_one", &[])
            .unwrap()
            .rows[0]["payload"],
        Value::Str("routed".into())
    );

    let ancestor_bound = engine
        .copy_from(
            "COPY nested_copy_local_one FROM STDIN",
            b"3\t20\t1\twould-rollback\n2\t150\t1\tinvalid\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(ancestor_bound.sqlstate(), Some("23514"));
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM nested_copy_root", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );

    exec(&engine, "BEGIN");
    let format = engine
        .copy_from(
            "COPY nested_copy_root FROM STDIN WITH (FORMAT csv)",
            b"4,30,1,\"unterminated\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(format.sqlstate(), Some("22P04"));
    let aborted = engine.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(aborted.sqlstate(), Some("25P02"));
    exec(&engine, "ROLLBACK");

    let header = engine
        .copy_from(
            "COPY nested_copy_root (id, region, bucket, payload) FROM STDIN WITH (FORMAT csv, HEADER MATCH)",
            b"wrong,region,bucket,payload\n5,40,1,nope\n".as_slice(),
        )
        .unwrap_err();
    assert_eq!(header.sqlstate(), Some("22P04"));
    let encoding = engine
        .copy_from("COPY nested_copy_root FROM STDIN", [0xff_u8].as_slice())
        .unwrap_err();
    assert_eq!(encoding.sqlstate(), Some("22021"));
}

#[test]
fn list_hash_default_and_csv_copy_keep_every_physical_row() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_lists (kind TEXT, payload TEXT DEFAULT 'list-default') PARTITION BY LIST (kind)",
    );
    exec(
        &engine,
        "CREATE TABLE copy_lists_known PARTITION OF copy_lists FOR VALUES IN ('a', 'b')",
    );
    exec(
        &engine,
        "CREATE TABLE copy_lists_default PARTITION OF copy_lists DEFAULT",
    );
    assert_eq!(
        engine
            .copy_from(
                "COPY copy_lists (kind, payload) FROM STDIN WITH (FORMAT csv, HEADER MATCH)",
                b"kind,payload\na,plain\nother,\"line\nvalue\"\n,\"\"\n".as_slice(),
            )
            .unwrap(),
        3
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM copy_lists_known", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM copy_lists_default", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT kind, payload FROM copy_lists ORDER BY kind NULLS LAST) TO STDOUT WITH (FORMAT csv, HEADER)",
        ),
        "kind,payload\na,plain\nother,\"line\nvalue\"\n,\"\"\n"
    );

    exec(
        &engine,
        "CREATE TABLE copy_hashes (key INTEGER, payload TEXT DEFAULT 'hash-default') PARTITION BY HASH (key)",
    );
    exec(
        &engine,
        "CREATE TABLE copy_hashes_0 PARTITION OF copy_hashes FOR VALUES WITH (MODULUS 2, REMAINDER 0)",
    );
    exec(
        &engine,
        "CREATE TABLE copy_hashes_1 PARTITION OF copy_hashes FOR VALUES WITH (MODULUS 2, REMAINDER 1)",
    );
    assert_eq!(
        engine
            .copy_from(
                "COPY copy_hashes (key) FROM STDIN",
                b"1\n2\n3\n4\n\\N\n".as_slice(),
            )
            .unwrap(),
        5
    );
    let physical = engine
        .sql(
            "SELECT _doc_id, key FROM copy_hashes ORDER BY key NULLS LAST",
            &[],
        )
        .unwrap();
    assert_eq!(physical.rows.len(), 5);
    assert!(physical.rows.iter().enumerate().any(|(index, row)| {
        physical.rows[index + 1..]
            .iter()
            .any(|other| row["_doc_id"] == other["_doc_id"])
    }));
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT key, payload FROM copy_hashes ORDER BY key NULLS LAST) TO STDOUT",
        )
        .lines()
        .count(),
        5
    );
}

#[test]
fn partition_copy_routing_and_identity_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("copy-hierarchy.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE persistent_copy (id SERIAL, bucket INTEGER, payload TEXT) PARTITION BY RANGE (bucket)",
        );
        exec(
            &engine,
            "CREATE TABLE persistent_copy_low PARTITION OF persistent_copy FOR VALUES FROM (0) TO (10)",
        );
        exec(
            &engine,
            "CREATE TABLE persistent_copy_high PARTITION OF persistent_copy FOR VALUES FROM (10) TO (20)",
        );
        assert_eq!(
            engine
                .copy_from(
                    "COPY persistent_copy (bucket, payload) FROM STDIN",
                    b"1\tone\n11\ttwo\n".as_slice(),
                )
                .unwrap(),
            2
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        copy_to_string(
            &reopened,
            "COPY (SELECT id, bucket, payload FROM persistent_copy ORDER BY id) TO STDOUT",
        ),
        "1\t1\tone\n2\t11\ttwo\n"
    );
    reopened
        .copy_from(
            "COPY persistent_copy_high (bucket, payload) FROM STDIN",
            b"12\tthree\n".as_slice(),
        )
        .unwrap();
    let row = reopened
        .sql(
            "SELECT id, payload FROM persistent_copy_high WHERE payload = 'three'",
            &[],
        )
        .unwrap();
    assert_eq!(row.rows[0]["id"], Value::Int(3));
}

#[test]
fn persistent_copy_batches_clustered_postings_and_survives_reopen() {
    use std::fmt::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("copy-postings.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE posting_copy (id BIGINT PRIMARY KEY, body TEXT)",
        );
        exec(
            &engine,
            "CREATE INDEX posting_copy_body_gin ON posting_copy USING gin (body)",
        );
        let mut input = String::new();
        for id in 1..=2_405 {
            let suffix = if id == 2_405 {
                "uniquezulu"
            } else {
                "ordinary"
            };
            writeln!(input, "{id}\tshared {suffix}").unwrap();
        }
        assert_eq!(
            engine
                .copy_from("COPY posting_copy (id, body) FROM STDIN", input.as_bytes(),)
                .unwrap(),
            2_405
        );
        assert_eq!(
            engine
                .sql(
                    "SELECT count(*) AS n FROM posting_copy WHERE text_match(body, 'shared')",
                    &[],
                )
                .unwrap()
                .rows[0]["n"],
            Value::Int(2_405)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    let unique = reopened
        .sql(
            "SELECT id FROM posting_copy WHERE text_match(body, 'uniquezulu')",
            &[],
        )
        .unwrap();
    assert_eq!(unique.rows.len(), 1);
    assert_eq!(unique.rows[0]["id"], Value::Int(2_405));
    assert_eq!(
        reopened
            .sql(
                "SELECT count(*) AS n FROM posting_copy WHERE text_match(body, 'shared')",
                &[],
            )
            .unwrap()
            .rows[0]["n"],
        Value::Int(2_405)
    );
}
