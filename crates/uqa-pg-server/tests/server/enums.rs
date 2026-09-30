//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum values, diagnostics and result metadata over the `PostgreSQL` wire protocol.

use serde_json::json;

use super::client::{compare_oracle, evidence, evidence_with_fields, Fixture};

#[test]
fn enum_oracle_matches_postgresql_over_tcp() {
    compare_oracle(include_str!(
        "../../../../tests/parity/pg18/enum_types_oracle.expected.json"
    ));
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
