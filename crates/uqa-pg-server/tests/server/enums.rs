//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum values, diagnostics and result metadata over the `PostgreSQL` wire protocol.

use serde_json::{json, Value};

use super::client::{error_matches, evidence, evidence_with_fields, Fixture};

/// User-defined type OIDs are database-local, so the oracle and the server compare them by class.
const FIRST_USER_OID: i64 = 16_384;

fn normalized(mut evidence: Value) -> Value {
    for result in evidence["results"].as_array_mut().into_iter().flatten() {
        for oid in result["type_oids"].as_array_mut().into_iter().flatten() {
            // The wire field is an unsigned OID; the test client reads it as a signed 32-bit integer.
            if oid
                .as_i64()
                .map(|value| if value < 0 { value + (1 << 32) } else { value })
                .is_some_and(|value| value >= FIRST_USER_OID)
            {
                *oid = json!("user-defined");
            }
        }
    }
    evidence
}

#[test]
fn enum_oracle_matches_postgresql_over_tcp() {
    let fixture = Fixture::new();
    let mut client = fixture.connect();
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/enum_types_oracle.expected.json"
    ))
    .unwrap();
    let mut differences = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let actual = normalized(evidence(&client.query(sql)));
        let expected = normalized(case.clone());
        for key in ["command_tags", "error", "results"] {
            if key == "results" && sql == "SELECT version()" {
                continue;
            }
            let matches = if key == "error" {
                error_matches(&actual[key], &expected[key])
            } else {
                actual[key] == expected[key]
            };
            if !matches {
                differences.push(format!(
                    "{sql}\n{key}: expected {}\nactual: {}",
                    expected[key], actual[key]
                ));
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn enum_result_fields_name_the_catalog_types() {
    let fixture = Fixture::new();
    let mut client = fixture.connect();
    for statement in [
        "CREATE TYPE mood AS ENUM ('sad', 'happy')",
        "CREATE TABLE person (m mood, ms mood[])",
        "INSERT INTO person VALUES ('happy', '{sad,happy}')",
    ] {
        let response = evidence(&client.query(statement));
        assert!(response["error"].is_null(), "{statement}: {response}");
    }
    let catalog =
        evidence(&client.query("SELECT oid, typarray FROM pg_type WHERE typname = 'mood'"));
    let row = &catalog["results"][0]["rows"][0];
    // RowDescription carries each OID's 32-bit pattern, which the test client reads as signed.
    let signed = |text: &str| i64::from(text.parse::<u32>().unwrap() as i32);
    let oid = signed(row[0].as_str().unwrap());
    let array_oid = signed(row[1].as_str().unwrap());
    let result = evidence_with_fields(&client.query("SELECT m, ms FROM person"));
    let fields = &result["results"][0]["fields"];
    assert_eq!(fields[0]["type_oid"], json!(oid));
    assert_eq!(fields[0]["type_size"], json!(4));
    assert_eq!(fields[0]["type_modifier"], json!(-1));
    assert_eq!(fields[1]["type_oid"], json!(array_oid));
    assert_eq!(fields[1]["type_size"], json!(-1));
    assert_eq!(
        result["results"][0]["rows"],
        json!([["happy", "{sad,happy}"]])
    );
}
