//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single integer primary key names the identity of each row whose key lies in `0..2^62`, so a key lookup reads that one identity. Rows with negative keys and keys at or above that limit take identities their table generates above it, where no key names one, so they never meet the row of another key; their keys resolve through the key's index. The expected rows and errors are those of `PostgreSQL` 18.4.

use super::*;

const LIMIT: i64 = 1 << 62;

/// Run `scenario` on a memory engine and on each persistent layout, then `reopened` on each layout after the engine that ran the scenario closed.
fn on_every_engine(scenario: impl Fn(&Engine, &str), reopened: impl Fn(&Engine, &str)) {
    scenario(&Engine::new(), "memory");
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("integer-keys.db");
        let label = format!("{layout:?}");
        scenario(&open(layout, &path), &label);
        reopened(&open(layout, &path), &label);
    }
}

/// Every row of `table` has the identity its key names when the key lies in `0..LIMIT`, and an identity at or above `LIMIT` otherwise.
fn assert_key_identities(engine: &Engine, table: &str, label: &str) {
    let result = engine
        .sql(&format!("SELECT id, _doc_id FROM {table}"), &[])
        .unwrap();
    for row in &result.rows {
        let (Some(Value::Int(key)), Some(Value::Int(identity))) =
            (row.get("id"), row.get("_doc_id"))
        else {
            panic!("{label}: unexpected row {row:?}");
        };
        if (0..LIMIT).contains(key) {
            assert_eq!(identity, key, "{label}: key {key}");
        } else {
            assert!(
                *identity >= LIMIT,
                "{label}: key {key} has identity {identity}"
            );
        }
    }
}

fn keyed_rows(engine: &Engine, label: &str) {
    let ordered = [
        i64::MIN.to_string(),
        "-1".into(),
        "1".into(),
        "2".into(),
        "3".into(),
        LIMIT.to_string(),
        i64::MAX.to_string(),
    ];
    assert_eq!(
        rows(engine, "SELECT id FROM keyed ORDER BY id", &["id"]),
        ordered,
        "{label}"
    );
    assert_eq!(
        rows(engine, "SELECT id FROM keyed ORDER BY id LIMIT 1", &["id"]),
        [i64::MIN.to_string()],
        "{label}"
    );
    assert_eq!(
        rows(
            engine,
            "SELECT id FROM keyed ORDER BY id DESC LIMIT 2",
            &["id"]
        ),
        [i64::MAX.to_string(), LIMIT.to_string()],
        "{label}"
    );
    for (key, value) in [("-1", "3"), ("3", "4"), ("4611686018427387904", "5")] {
        assert_eq!(
            rows(
                engine,
                &format!("SELECT v FROM keyed WHERE id = {key}"),
                &["v"]
            ),
            [value],
            "{label}: {key}"
        );
        assert_eq!(
            state(engine, &format!("INSERT INTO keyed VALUES ({key}, 9)")).as_deref(),
            Some("23505"),
            "{label}: {key}"
        );
    }
    assert_key_identities(engine, "keyed", label);
}

#[test]
fn keys_outside_the_named_range_keep_their_rows_order_and_lookups() {
    on_every_engine(
        |engine, label| {
            run(
                engine,
                "CREATE TABLE keyed (id bigint PRIMARY KEY, v integer)",
            );
            run(engine, "INSERT INTO keyed VALUES (1, 1), (2, 2)");
            run(engine, "INSERT INTO keyed VALUES (-1, 3)");
            // The identity a negative key took never belongs to a later key.
            run(engine, "INSERT INTO keyed VALUES (3, 4)");
            run(
                engine,
                "INSERT INTO keyed VALUES (4611686018427387904, 5), (9223372036854775807, 6), (-9223372036854775808, 7)",
            );
            keyed_rows(engine, label);
        },
        keyed_rows,
    );
}

fn reference_rows(engine: &Engine, label: &str) {
    assert_eq!(
        state(engine, "INSERT INTO children VALUES (3)").as_deref(),
        Some("23503"),
        "{label}"
    );
    assert_eq!(
        state(engine, "DELETE FROM parents WHERE id = -1").as_deref(),
        Some("23503"),
        "{label}"
    );
    assert_eq!(
        rows(engine, "SELECT parent FROM children", &["parent"]),
        ["-1"],
        "{label}"
    );
}

#[test]
fn foreign_keys_find_exactly_the_parents_their_keys_name() {
    on_every_engine(
        |engine, label| {
            run(
                engine,
                "CREATE TABLE parents (id integer PRIMARY KEY);
                 INSERT INTO parents VALUES (1), (2), (-1);
                 CREATE TABLE children (parent integer REFERENCES parents (id))",
            );
            run(engine, "INSERT INTO children VALUES (-1)");
            reference_rows(engine, label);
        },
        reference_rows,
    );
}

fn moved_rows(engine: &Engine, label: &str) {
    assert_eq!(
        rows(engine, "SELECT id, v FROM moved ORDER BY id", &["id", "v"]),
        ["-5|5", "1|2", "2|1", "5|6"],
        "{label}"
    );
    assert_eq!(
        rows(engine, "SELECT v FROM moved WHERE id = 5", &["v"]),
        ["6"],
        "{label}"
    );
    assert_key_identities(engine, "moved", label);
}

#[test]
fn updates_move_rows_between_key_named_and_generated_identities() {
    on_every_engine(
        |engine, label| {
            run(
                engine,
                "CREATE TABLE moved (id integer PRIMARY KEY, v integer);
                 INSERT INTO moved VALUES (1, 1), (2, 2), (5, 5)",
            );
            // The row leaving key 1 for -1 vacates the identity key 1 names, which the other row takes.
            run(
                engine,
                "UPDATE moved SET id = CASE id WHEN 1 THEN -1 WHEN 2 THEN 1 END WHERE id IN (1, 2)",
            );
            assert_eq!(
                rows(engine, "SELECT id, v FROM moved ORDER BY v", &["id", "v"]),
                ["-1|1", "1|2", "5|5"],
                "{label}"
            );
            run(engine, "UPDATE moved SET id = 2 WHERE id = -1");
            run(engine, "UPDATE moved SET id = -5 WHERE id = 5");
            run(engine, "INSERT INTO moved VALUES (5, 6)");
            moved_rows(engine, label);
        },
        moved_rows,
    );
}

fn sequence_key_rows(engine: &Engine, label: &str) {
    assert_eq!(
        rows(
            engine,
            "SELECT id, v FROM serial_keys ORDER BY id",
            &["id", "v"]
        ),
        ["-3|1", "1|2"],
        "{label}"
    );
    assert_eq!(
        rows(
            engine,
            "SELECT id, v FROM identity_keys ORDER BY id",
            &["id", "v"]
        ),
        ["-4|1", "1|2"],
        "{label}"
    );
}

#[test]
fn negative_serial_and_identity_keys_are_ordinary_values() {
    on_every_engine(
        |engine, label| {
            run(
                engine,
                "CREATE TABLE serial_keys (id serial PRIMARY KEY, v integer);
                 CREATE TABLE identity_keys (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, v integer)",
            );
            run(engine, "INSERT INTO serial_keys (id, v) VALUES (-3, 1)");
            run(engine, "INSERT INTO serial_keys (v) VALUES (2)");
            run(engine, "INSERT INTO identity_keys VALUES (-4, 1)");
            run(engine, "INSERT INTO identity_keys (v) VALUES (2)");
            sequence_key_rows(engine, label);
        },
        sequence_key_rows,
    );
}

#[test]
fn a_negative_key_and_the_next_key_insert_concurrently_without_conflict() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("integer-key-sessions.db");
        // redb has one physical owner per file, which independent engines share.
        let shared = matches!(layout, Layout::Redb).then(|| provider(layout, &path));
        let session = || match &shared {
            Some(provider) => Engine::from_persistent_provider(provider.clone()).unwrap(),
            None => open(layout, &path),
        };
        let first = session();
        run(
            &first,
            "CREATE TABLE racing (id integer PRIMARY KEY, v integer);
             INSERT INTO racing VALUES (1, 1)",
        );
        let second = session();
        // The negative key's identity is generated above every key-named identity, so it is never the identity of the key the other session supplies.
        run(&first, "BEGIN; INSERT INTO racing VALUES (-1, 2)");
        run(&second, "INSERT INTO racing VALUES (2, 3)");
        run(&first, "COMMIT");
        assert_eq!(
            rows(
                &second,
                "SELECT id, v FROM racing ORDER BY id",
                &["id", "v"]
            ),
            ["-1|2", "1|1", "2|3"],
            "{layout:?}"
        );
    }
}
