//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn target_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE acl_target_owner",
        "CREATE ROLE acl_target_user",
        "CREATE ROLE acl_target_outsider",
        "CREATE SCHEMA acl_target_space",
        "GRANT CREATE ON SCHEMA public TO acl_target_owner",
        "GRANT USAGE, CREATE ON SCHEMA acl_target_space TO acl_target_owner",
        "GRANT USAGE ON SCHEMA acl_target_space TO acl_target_user",
        "SET ROLE acl_target_owner",
        "CREATE SEQUENCE acl_legacy_target",
        "CREATE SEQUENCE acl_target_space.acl_scoped_target",
        "CREATE TABLE acl_target_table (id integer)",
        "GRANT USAGE ON TABLE acl_legacy_target TO acl_target_user GRANTED BY CURRENT_USER",
        "GRANT ALL PRIVILEGES ON SEQUENCE acl_target_space.acl_scoped_target TO acl_target_user",
        "GRANT SELECT ON ALL SEQUENCES IN SCHEMA public, acl_target_space TO PUBLIC",
        "CREATE SEQUENCE acl_late_target",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn assert_target_privileges(engine: &Engine) {
    for (role, sequence, privilege, expected) in [
        ("acl_target_user", "acl_legacy_target", "USAGE", true),
        (
            "acl_target_user",
            "acl_target_space.acl_scoped_target",
            "SELECT, UPDATE, USAGE",
            true,
        ),
        ("acl_target_outsider", "acl_legacy_target", "SELECT", true),
        ("acl_target_outsider", "acl_late_target", "SELECT", false),
    ] {
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_sequence_privilege('{role}', '{sequence}', '{privilege}') AS v"
                ),
            ),
            Value::Bool(expected),
            "{role} {sequence} {privilege}"
        );
    }
}

fn assert_target_error_precedence(engine: &Engine) {
    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "GRANT INSERT ON SEQUENCE acl_missing_target TO acl_missing_role",
        ),
        "25006"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    for (sql, expected) in [
        (
            "GRANT INSERT ON SEQUENCE acl_missing_target TO acl_missing_role",
            "42P01",
        ),
        (
            "GRANT INSERT ON SEQUENCE acl_legacy_target TO acl_missing_role",
            "42704",
        ),
        (
            "GRANT INSERT ON SEQUENCE acl_legacy_target TO acl_target_user",
            "0LP01",
        ),
        (
            "GRANT SELECT (value) ON SEQUENCE acl_legacy_target TO acl_target_user",
            "0LP01",
        ),
        (
            "GRANT USAGE ON SEQUENCE acl_target_table TO acl_missing_role",
            "42704",
        ),
        (
            "GRANT USAGE ON SEQUENCE acl_target_table TO acl_target_user",
            "42809",
        ),
        (
            "GRANT USAGE ON ALL SEQUENCES IN SCHEMA acl_missing_schema TO acl_target_user",
            "3F000",
        ),
        (
            "GRANT USAGE ON SEQUENCE acl_legacy_target TO PUBLIC WITH GRANT OPTION",
            "0LP01",
        ),
    ] {
        assert_eq!(sqlstate(engine, sql), expected, "{sql}");
    }
    engine.sql("SET ROLE acl_target_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "GRANT USAGE ON SEQUENCE acl_legacy_target TO acl_target_user GRANTED BY acl_target_user",
        ),
        "0A000"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

#[test]
fn sequence_acl_targets_public_and_error_precedence_match_postgresql() {
    let engine = target_engine();
    assert_target_privileges(&engine);
    assert_target_error_precedence(&engine);
}
