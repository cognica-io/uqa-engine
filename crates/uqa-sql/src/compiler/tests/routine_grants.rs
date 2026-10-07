//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterRoutineKind, GrantRoutineStmt, RoutinePrivilege, RoutineRevokeBehavior};

fn grant(sql: &str) -> GrantRoutineStmt {
    let Statement::GrantRoutine(statement) = first(sql) else {
        panic!("routine grant");
    };
    statement
}

#[test]
fn routine_schema_targets_preserve_kind_written_order_and_acl_options() {
    for (word, kind) in [
        ("FUNCTIONS", AlterRoutineKind::Function),
        ("PROCEDURES", AlterRoutineKind::Procedure),
        ("ROUTINES", AlterRoutineKind::Routine),
    ] {
        let statement = grant(&format!("REVOKE GRANT OPTION FOR EXECUTE ON ALL {word} IN SCHEMA app, \"Mixed.Schema\", app FROM PUBLIC, CURRENT_USER CASCADE"));
        assert_eq!(statement.kind, kind);
        assert_eq!(
            statement.schemas.as_deref().unwrap(),
            ["app", "Mixed.Schema", "app"]
        );
        assert_eq!(statement.items.len(), 0);
        assert!(!statement.is_grant);
        assert!(statement.grant_option_only);
        assert_eq!(statement.revoke_behavior, RoutineRevokeBehavior::Cascade);
        assert_eq!(statement.privileges, [RoutinePrivilege::Execute]);
        let encoded = serde_json::to_string(&statement).unwrap();
        assert_eq!(
            serde_json::from_str::<GrantRoutineStmt>(&encoded).unwrap(),
            statement
        );
    }
}

#[test]
fn routine_grants_read_legacy_explicit_targets_without_new_fields() {
    let statement = grant("GRANT EXECUTE ON FUNCTION app.f(integer) TO PUBLIC");
    let mut legacy = serde_json::to_value(&statement).unwrap();
    legacy.as_object_mut().unwrap().remove("schemas");
    legacy.as_object_mut().unwrap().remove("privileges");
    let restored: GrantRoutineStmt = serde_json::from_value(legacy).unwrap();
    assert!(restored.schemas.is_none());
    assert_eq!(restored.privileges.len(), 0);
    assert_eq!(restored.items, statement.items);
    assert_eq!(restored.grantees, statement.grantees);
}

#[test]
fn routine_privilege_errors_are_deferred_until_target_and_role_lookup() {
    // PostgreSQL 18.4 reports missing schema first; a valid target reaches 0LP01.
    for (privilege, expected) in [
        ("SELECT", RoutinePrivilege::Unsupported("select".into())),
        ("EXECUTE(a)", RoutinePrivilege::ColumnsUnsupported),
    ] {
        let statement = grant(&format!(
            "GRANT {privilege} ON ALL FUNCTIONS IN SCHEMA missing TO PUBLIC"
        ));
        assert_eq!(statement.privileges, [expected]);
        assert_eq!(statement.schemas.as_deref().unwrap(), ["missing"]);
    }
}
