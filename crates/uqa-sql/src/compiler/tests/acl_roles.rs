//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Object ACL and owner declarations preserve quoted names through stored statements.

use super::first;
use crate::ast::{AclRoleSpecification, RoleSpecification, Statement};

fn grant_roles(statement: Statement) -> (Vec<AclRoleSpecification>, Option<RoleSpecification>) {
    match statement {
        Statement::GrantTable(grant) => (grant.grantees, grant.grantor),
        Statement::GrantSequence(grant) => (grant.grantees, grant.grantor),
        Statement::GrantDatabase(grant) => (grant.grantees, grant.grantor),
        Statement::GrantSchema(grant) => (grant.grantees, grant.grantor),
        Statement::GrantRoutine(grant) => (grant.grantees, grant.grantor),
        _ => panic!("expected an object privilege statement"),
    }
}

#[test]
fn object_grantees_and_grantors_keep_keywords_separate_from_quoted_role_names() {
    for target in [
        "SELECT ON TABLE items",
        "USAGE ON SEQUENCE ids",
        "CONNECT ON DATABASE uqa",
        "USAGE ON SCHEMA app",
        "EXECUTE ON FUNCTION f()",
    ] {
        for (command, direction) in [("GRANT", "TO"), ("REVOKE", "FROM")] {
            let statement = first(&format!(
                r#"{command} {target} {direction} PUBLIC, "PUBLIC", CURRENT_USER, "CURRENT_USER", SESSION_USER, "SESSION_USER" GRANTED BY "CURRENT_USER""#
            ));
            let stored = serde_json::to_string(&statement).unwrap();
            let expected = (
                vec![
                    AclRoleSpecification::Public,
                    "PUBLIC".into(),
                    RoleSpecification::CurrentUser.into(),
                    "CURRENT_USER".into(),
                    RoleSpecification::SessionUser.into(),
                    "SESSION_USER".into(),
                ],
                Some("CURRENT_USER".into()),
            );
            assert_eq!(grant_roles(statement), expected, "{command} {target}");
            assert_eq!(
                grant_roles(serde_json::from_str(&stored).unwrap()),
                expected,
                "stored {command} {target}"
            );
        }
    }
}

fn requested_owner(statement: Statement) -> RoleSpecification {
    use crate::ast::{AlterForeignTableAction, AlterTableAction, AlterViewAction};
    match statement {
        Statement::AlterTable(table) => match table.actions.into_iter().next().unwrap() {
            AlterTableAction::ChangeOwner { owner } => owner,
            _ => panic!("expected an ordinary table owner"),
        },
        Statement::AlterForeignTable(table) => match table.action {
            AlterForeignTableAction::OwnerTo(owner) => owner,
            _ => panic!("expected a foreign table owner"),
        },
        Statement::AlterView(view) => match view.action {
            AlterViewAction::OwnerTo(owner) => owner,
            _ => panic!("expected a view owner"),
        },
        Statement::AlterSequence(sequence) => sequence.role_owner.unwrap(),
        Statement::AlterSchemaOwner { new_owner, .. } => new_owner,
        Statement::AlterRoutineOwner(routine) => routine.new_owner,
        _ => panic!("expected an owner declaration"),
    }
}

#[test]
fn all_object_owner_declarations_preserve_quoted_session_role_names() {
    for target in [
        "TABLE items",
        "FOREIGN TABLE remote",
        "VIEW visible",
        "MATERIALIZED VIEW saved",
        "SEQUENCE ids",
        "SCHEMA app",
        "FUNCTION f()",
        "PROCEDURE p()",
    ] {
        for (syntax, expected) in [
            (r#""PUBLIC""#, RoleSpecification::from("PUBLIC")),
            (r#""CURRENT_USER""#, RoleSpecification::from("CURRENT_USER")),
            (r#""SESSION_USER""#, RoleSpecification::from("SESSION_USER")),
            ("CURRENT_USER", RoleSpecification::CurrentUser),
            ("CURRENT_ROLE", RoleSpecification::CurrentUser),
            ("SESSION_USER", RoleSpecification::SessionUser),
        ] {
            let statement = first(&format!("ALTER {target} OWNER TO {syntax}"));
            let stored = serde_json::to_string(&statement).unwrap();
            assert_eq!(requested_owner(statement), expected, "{target} {syntax}");
            assert_eq!(
                requested_owner(serde_json::from_str(&stored).unwrap()),
                expected,
                "stored {target} {syntax}"
            );
        }
    }
}
