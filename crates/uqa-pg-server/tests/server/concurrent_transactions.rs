//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! TCP sessions must finish independent writes while another connection retains private changes.

use serde_json::{json, Value};

use super::client::{evidence, Client, Fixture};

const OBSERVE: &str = "SELECT id, value FROM records ORDER BY id";

fn query(client: &mut Client, sql: &str, status: u8, command_tag: &str) -> Value {
    let messages = client.query(sql);
    let actual = evidence(&messages);
    assert!(actual["error"].is_null(), "{sql}: {actual}");
    assert_eq!(actual["command_tags"], json!([command_tag]), "{sql}");
    assert_eq!(messages.last().unwrap(), &(b'Z', vec![status]), "{sql}");
    actual
}

fn step(client: &mut Client, sql: &str) {
    let (status, tag) = match sql.split_whitespace().next().unwrap() {
        "CREATE" => (b'I', "CREATE TABLE"),
        "INSERT" => (b'I', "INSERT 0 2"),
        "BEGIN" => (b'T', "BEGIN"),
        "UPDATE" => (b'T', "UPDATE 1"),
        "SAVEPOINT" => (b'T', "SAVEPOINT"),
        "COMMIT" => (b'I', "COMMIT"),
        "ROLLBACK" if sql.starts_with("ROLLBACK TO ") => (b'T', "ROLLBACK"),
        "ROLLBACK" => (b'I', "ROLLBACK"),
        _ => panic!("unexpected reference statement: {sql}"),
    };
    query(client, sql, status, tag);
}

fn steps(client: &mut Client, statements: &Value) {
    for statement in statements.as_array().unwrap() {
        step(client, statement.as_str().unwrap());
    }
}

fn rows(client: &mut Client, sql: &str, status: u8) -> Value {
    let result = query(client, sql, status, "SELECT 2");
    assert_eq!(result["results"][0]["columns"], json!(["id", "value"]));
    assert_eq!(result["results"][0]["type_oids"], json!([23, 23]));
    json!(result["results"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("|"))
        .collect::<Vec<_>>())
}

fn setup(client: &mut Client) {
    step(
        client,
        "CREATE TABLE records(id INT PRIMARY KEY, value INT)",
    );
    step(client, "INSERT INTO records VALUES (1,0),(2,0)");
}

fn independent_commit(client: &mut Client) {
    step(client, "BEGIN");
    step(client, "UPDATE records SET value=20 WHERE id=2");
    step(client, "COMMIT");
}

#[test]
fn independent_tcp_writers_follow_postgresql_commit_rollback_and_savepoint_schedules() {
    let reference: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/concurrent_writes.expected.json"
    ))
    .unwrap();
    let observe = reference["observe"].as_str().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let fixture = Fixture::new();
        let mut first = fixture.connect();
        let mut second = fixture.connect();
        steps(&mut first, &reference["setup"]);
        steps(&mut first, &case["a_before"]);

        // The client's existing socket timeout bounds a blocked writer. A stays open until B returns its COMMIT completion and idle status.
        steps(&mut second, &case["b"]);
        assert_eq!(
            rows(&mut second, observe, b'I'),
            case["before_a_end"],
            "{}",
            case["name"]
        );
        assert_eq!(rows(&mut first, observe, b'T'), json!(["1|10", "2|20"]));
        steps(&mut first, &case["a_finish"]);
        for client in [&mut first, &mut second] {
            assert_eq!(
                rows(client, observe, b'I'),
                case["after_a_end"],
                "{}",
                case["name"]
            );
        }
        assert_eq!(
            rows(&mut fixture.connect(), observe, b'I'),
            case["after_a_end"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn tcp_isolation_preserves_private_writes_and_refreshes_only_command_snapshots() {
    for (isolation, peer_value) in [
        ("READ UNCOMMITTED", "2|20"),
        ("READ COMMITTED", "2|20"),
        ("REPEATABLE READ", "2|0"),
        ("SERIALIZABLE", "2|0"),
    ] {
        let fixture = Fixture::new();
        let mut first = fixture.connect();
        let mut second = fixture.connect();
        setup(&mut first);
        step(&mut first, &format!("BEGIN ISOLATION LEVEL {isolation}"));
        step(&mut first, "UPDATE records SET value=10 WHERE id=1");
        assert_eq!(rows(&mut first, OBSERVE, b'T'), json!(["1|10", "2|0"]));

        independent_commit(&mut second);
        assert_eq!(rows(&mut second, OBSERVE, b'I'), json!(["1|0", "2|20"]));
        assert_eq!(
            rows(&mut first, OBSERVE, b'T'),
            json!(["1|10", peer_value]),
            "{isolation}"
        );
        step(&mut first, "COMMIT");
        for client in [&mut first, &mut second] {
            assert_eq!(
                rows(client, OBSERVE, b'I'),
                json!(["1|10", "2|20"]),
                "{isolation}"
            );
        }
    }
}

#[test]
fn disconnect_rolls_back_a_private_writer_without_losing_an_independent_commit() {
    let fixture = Fixture::new();
    let mut first = fixture.connect();
    let mut second = fixture.connect();
    setup(&mut first);
    step(&mut first, "BEGIN");
    step(&mut first, "UPDATE records SET value=10 WHERE id=1");
    independent_commit(&mut second);
    assert_eq!(rows(&mut second, OBSERVE, b'I'), json!(["1|0", "2|20"]));

    drop(first);
    query(
        &mut second,
        "UPDATE records SET value=value+30 WHERE id=1",
        b'I',
        "UPDATE 1",
    );
    assert_eq!(rows(&mut second, OBSERVE, b'I'), json!(["1|30", "2|20"]));
    assert_eq!(
        rows(&mut fixture.connect(), OBSERVE, b'I'),
        json!(["1|30", "2|20"])
    );
}
