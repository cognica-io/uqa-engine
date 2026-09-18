//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn inquiry_engine() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE ROLE reader; CREATE SEQUENCE ids; CREATE TABLE ordinary (v integer); CREATE SCHEMA hidden; CREATE SEQUENCE hidden.ids; GRANT USAGE ON SEQUENCE ids TO PUBLIC", &[]).unwrap();
    engine
}

#[test]
fn sequence_inquiry_validates_roles_then_privileges_before_resolving_targets() {
    let engine = inquiry_engine();
    for subject in [
        "",
        "'reader', ",
        "(SELECT oid FROM pg_roles WHERE rolname = 'reader'), ",
    ] {
        for target in [
            "'missing'",
            "0::oid",
            "'ordinary'",
            "'ordinary'::regclass::oid",
            "'missing_schema.ids'",
            "'hidden.ids'",
        ] {
            let statement = format!("SELECT has_sequence_privilege({subject}{target}, 'bad')");
            assert_eq!(sqlstate(&engine, &statement), "22023", "{statement}");
            let statement =
                format!("SELECT has_sequence_privilege('missing_role', {target}, 'bad')");
            assert_eq!(sqlstate(&engine, &statement), "42704", "{statement}");
        }
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege(NULL::name, 'missing', 'bad') AS v"
        ),
        Value::Null
    );
    engine.sql("SET ROLE reader", &[]).unwrap();
    for (privilege, expected) in [("bad", "22023"), ("USAGE", "42501")] {
        assert_eq!(
            sqlstate(
                &engine,
                &format!("SELECT has_sequence_privilege('hidden.ids', '{privilege}')")
            ),
            expected
        );
    }
}

#[test]
fn sequence_inquiry_public_and_missing_role_oids_retain_public_privileges() {
    let engine = inquiry_engine();
    for subject in ["'public'", "0::oid", "4294967295::oid"] {
        for target in ["'ids'", "'ids'::regclass::oid"] {
            for (privilege, expected) in [
                ("USAGE", true),
                ("SELECT", false),
                ("USAGE WITH GRANT OPTION", false),
            ] {
                let statement = format!(
                    "SELECT has_sequence_privilege({subject}, {target}, '{privilege}') AS v"
                );
                assert_eq!(
                    scalar(&engine, &statement),
                    Value::Bool(expected),
                    "{statement}"
                );
            }
        }
    }
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_sequence_privilege('PUBLIC', 'ids', 'USAGE')"
        ),
        "42704"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('reader', 0::oid, 'USAGE') AS v"
        ),
        Value::Null
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_sequence_privilege('reader', 'missing_schema.ids', 'USAGE')"
        ),
        "3F000"
    );
}
