//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn temporary_sequence_acl_follows_rename_transaction_and_discard() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE acl_temp_owner",
        "CREATE ROLE acl_temp_user",
        "SET ROLE acl_temp_owner",
        "CREATE TEMP SEQUENCE acl_temp_ids",
        "GRANT USAGE ON SEQUENCE acl_temp_ids TO acl_temp_user",
        "ALTER SEQUENCE acl_temp_ids RENAME TO acl_temp_renamed",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_temp_user', 'acl_temp_renamed', 'USAGE') AS v",
        ),
        Value::Bool(true)
    );
    for sql in [
        "SET ROLE acl_temp_owner",
        "BEGIN",
        "REVOKE USAGE ON SEQUENCE acl_temp_renamed FROM acl_temp_user",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_temp_user', 'acl_temp_renamed', 'USAGE') AS v",
        ),
        Value::Bool(false)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_temp_user', 'acl_temp_renamed', 'USAGE') AS v",
        ),
        Value::Bool(true)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("DISCARD TEMP", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_sequence_privilege('acl_temp_user', 'acl_temp_renamed', 'USAGE')",
        ),
        "42P01"
    );
}
