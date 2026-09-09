//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Write;
use std::net::TcpStream;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::{json, Value};
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};

use super::client::{evidence, evidence_with_fields, fields, Client, Fixture};

fn compare_reference(input: &str) {
    let fixture = Fixture::new();
    let mut client = fixture.connect();
    let reference: Value = serde_json::from_str(input).unwrap();
    let include_fields = reference["cases"][0]["results"][0].get("fields").is_some();
    let mut differences = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let response = client.query(sql);
        let actual = if include_fields {
            evidence_with_fields(&response)
        } else {
            evidence(&response)
        };
        for key in ["command_tags", "error", "results"] {
            if let Some(expected) = case.get(key) {
                if key == "results" && sql == "SELECT version()" {
                    continue;
                }
                if &actual[key] != expected {
                    differences.push(format!(
                        "{sql}\n{key}: expected {expected}\nactual: {}",
                        actual[key]
                    ));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn simple_query_messages_match_postgresql_over_tcp() {
    compare_reference(include_str!(
        "../../../../tests/parity/pg18/command_completion_oracle.expected.json"
    ));
}

#[test]
fn data_modifying_ctes_match_postgresql_over_tcp() {
    compare_reference(include_str!(
        "../../../../tests/parity/pg18/cte_commands_oracle.expected.json"
    ));
}

#[test]
fn command_cte_composition_matches_postgresql_over_tcp() {
    compare_reference(include_str!(
        "../../../../tests/parity/pg18/cte_command_composition_oracle.expected.json"
    ));
}

#[test]
fn result_fields_and_text_match_postgresql_over_tcp() {
    compare_reference(include_str!(
        "../../../../tests/parity/pg18/wire_result_oracle.expected.json"
    ));
}

#[test]
fn empty_descriptors_differ_from_command_only_results() {
    let fixture = Fixture::new();
    let mut client = fixture.connect();
    for (sql, expected) in [
        ("SELECT FROM (VALUES (1)) AS v", "TDCZ"),
        ("SELECT FROM (VALUES (1)) AS v WHERE false", "TCZ"),
        (
            "CREATE TABLE empty_descriptor AS SELECT 1 AS id WITH NO DATA",
            "CZ",
        ),
        ("CREATE TABLE populated_descriptor AS SELECT 1 AS id", "CZ"),
        ("; -- no query", "IZ"),
    ] {
        let messages = client.query(sql);
        let tags = messages
            .iter()
            .map(|(tag, _)| char::from(*tag))
            .collect::<String>();
        assert_eq!(tags, expected, "{sql}: {messages:?}");
    }
}

#[test]
fn startup_validates_database_and_login_role() {
    let fixture = Fixture::new();
    fixture
        .engine
        .sql(
            "CREATE ROLE wire_reader LOGIN; CREATE ROLE wire_no_login",
            &[],
        )
        .unwrap();
    for (user, database, state, text) in [
        ("missing", "uqa", "28000", "role \"missing\" does not exist"),
        (
            "wire_no_login",
            "uqa",
            "28000",
            "role \"wire_no_login\" is not permitted to log in",
        ),
        (
            "uqa",
            "missing",
            "3D000",
            "database \"missing\" does not exist",
        ),
    ] {
        let (_, response) = Client::connect(fixture.server.local_addr(), user, database, 196_608);
        let (_, error) = response.last().unwrap();
        let diagnostic = fields(error);
        assert_eq!(diagnostic[&b'C'], state);
        assert_eq!(diagnostic[&b'M'], text);
        assert_eq!(diagnostic[&b'S'], "FATAL");
    }
    let (mut reader, response) =
        Client::connect(fixture.server.local_addr(), "wire_reader", "uqa", 196_608);
    assert_eq!(response.last().unwrap().0, b'Z');
    assert_eq!(reader.key.len(), 8);
    let result = evidence(&reader.query("SELECT current_user, session_user"));
    assert_eq!(
        result["results"][0]["rows"],
        json!([["wire_reader", "wire_reader"]])
    );
    let result = evidence(&reader.query("SET ROLE uqa"));
    assert_eq!(result["error"]["sqlstate"], "42501");
    fixture
        .engine
        .sql("REVOKE CONNECT ON DATABASE uqa FROM PUBLIC", &[])
        .unwrap();
    let (_, response) = Client::connect(fixture.server.local_addr(), "wire_reader", "uqa", 196_610);
    assert_eq!(fields(&response.last().unwrap().1)[&b'C'], "42501");
}

#[test]
fn sessions_preserve_transactions_and_rollback_on_disconnect() {
    let fixture = Fixture::new();
    let mut first = fixture.connect();
    let mut second = fixture.connect();
    first.query("CREATE TABLE wire_state (id integer PRIMARY KEY)");
    let result = first.query("BEGIN; INSERT INTO wire_state VALUES (1)");
    assert_eq!(result.last().unwrap().1, b"T");
    assert_eq!(
        evidence(&second.query("SELECT * FROM wire_state"))["results"][0]["rows"],
        json!([])
    );
    let failed = first.query("SELECT missing FROM wire_state");
    assert_eq!(failed.last().unwrap().1, b"E");
    let failed = first.query("SELECT 1");
    assert_eq!(evidence(&failed)["error"]["sqlstate"], "25P02");
    let rolled_back = first.query("COMMIT");
    assert_eq!(evidence(&rolled_back)["command_tags"], json!(["ROLLBACK"]));
    assert_eq!(rolled_back.last().unwrap().1, b"I");
    first.query("BEGIN; INSERT INTO wire_state VALUES (2)");
    drop(first);
    second.query("INSERT INTO wire_state VALUES (2)");
    assert_eq!(
        evidence(&second.query("SELECT * FROM wire_state"))["results"][0]["rows"],
        json!([["2"]])
    );
}

#[test]
fn notifications_arrive_on_an_idle_connection() {
    let fixture = Fixture::new();
    let mut listener = fixture.connect();
    let mut sender = fixture.connect();
    listener.query("LISTEN wire_channel");
    sender.query("NOTIFY wire_channel, 'payload'");
    let (tag, payload) = listener.receive();
    assert_eq!(tag, b'A');
    assert_eq!(&payload[4..], b"wire_channel\0payload\0");
    assert_eq!(&payload[..4], &sender.key[..4]);
}

#[test]
fn cancellation_uses_the_session_key_and_recovers() {
    let fixture = Fixture::new();
    let (started, seen) = mpsc::sync_channel(1);
    let started = Arc::new(started);
    fixture
        .engine
        .register_scalar_function_with_options(
            "wire_started",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_args: &[uqa_core::Value]| {
                let _ = started.try_send(());
                Ok(uqa_core::Value::Int(1))
            },
        )
        .unwrap();
    let mut client = fixture.connect();
    assert_eq!(client.key.len(), 36);
    client.send(
        b'Q',
        b"SELECT * FROM generate_series(wire_started()::integer,1000000000)\0",
    );
    seen.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut cancellation = TcpStream::connect(fixture.server.local_addr()).unwrap();
    cancellation
        .write_all(&((client.key.len() + 8) as i32).to_be_bytes())
        .unwrap();
    cancellation
        .write_all(&80_877_102_i32.to_be_bytes())
        .unwrap();
    cancellation.write_all(&client.key).unwrap();
    drop(cancellation);
    let result = client.finish_query();
    assert_eq!(evidence(&result)["error"]["sqlstate"], "57014");
    assert_eq!(result.last().unwrap().1, b"I");
    assert_eq!(
        evidence(&client.query("SELECT 42"))["results"][0]["rows"],
        json!([["42"]])
    );
}
