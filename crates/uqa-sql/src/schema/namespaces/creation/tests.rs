//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition,
    },
    plan::{CommandPlan, UnifiedPlan},
    Statement,
};
use std::collections::BTreeMap;

#[test]
fn schema_authorization_preserves_implicit_names_keywords_and_quoted_role_literals() {
    for (sql, name, authorization) in [
        (
            "CREATE SCHEMA explicit AUTHORIZATION CURRENT_ROLE",
            Some("explicit"),
            SchemaAuthorization::CurrentUser,
        ),
        (
            "CREATE SCHEMA AUTHORIZATION CURRENT_USER",
            None,
            SchemaAuthorization::CurrentUser,
        ),
        (
            "CREATE SCHEMA IF NOT EXISTS AUTHORIZATION SESSION_USER",
            None,
            SchemaAuthorization::SessionUser,
        ),
        (
            r#"CREATE SCHEMA AUTHORIZATION "CURRENT_USER""#,
            None,
            SchemaAuthorization::Role("CURRENT_USER".into()),
        ),
    ] {
        let statement = crate::compile(sql).unwrap().remove(0);
        let Statement::CreateSchema {
            name: parsed,
            authorization: owner,
            ..
        } = &statement
        else {
            panic!("expected schema")
        };
        assert_eq!(parsed.as_deref(), name);
        assert_eq!(owner.as_ref(), Some(&authorization));
        let UnifiedPlan::Command(plan) = UnifiedPlan::lower(statement) else {
            panic!("expected command")
        };
        let CommandPlan::CreateSchema {
            name: planned,
            authorization: owner,
            ..
        } = *plan
        else {
            panic!("expected schema plan")
        };
        assert_eq!(planned.as_deref(), name);
        assert_eq!(owner, Some(authorization));
    }
}

#[test]
fn pre_authorization_schema_ast_and_plan_json_keep_their_original_representation() {
    let stored = serde_json::json!({"CreateSchema":{"name":"existing","if_not_exists":false}});
    let statement: Statement = serde_json::from_value(stored.clone()).unwrap();
    let plan: CommandPlan = serde_json::from_value(stored.clone()).unwrap();
    assert_eq!(serde_json::to_value(&statement).unwrap(), stored);
    assert_eq!(serde_json::to_value(&plan).unwrap(), stored);
}

struct Catalog(BTreeMap<String, RoleDefinition>);
impl RoleReferenceNames for Catalog {
    fn current_user_name(&self) -> String {
        panic!("caller already captured current role")
    }
    fn session_user_name(&self) -> String {
        "session_owner".into()
    }
}
impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.0)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        panic!("SET ROLE authority is checked after database privileges")
    }
}

#[test]
fn schema_target_binding_distinguishes_named_roles_from_current_and_session_owners() {
    let catalog = Catalog(
        ["active_owner", "session_owner", "CURRENT_USER"]
            .into_iter()
            .map(|name| (name.into(), RoleDefinition::bootstrap()))
            .collect(),
    );
    for (authorization, expected) in [
        (SchemaAuthorization::CurrentUser, "active_owner"),
        (SchemaAuthorization::SessionUser, "session_owner"),
        (
            SchemaAuthorization::Role("CURRENT_USER".into()),
            "CURRENT_USER",
        ),
    ] {
        let target = schema_creation_target(
            &catalog,
            &catalog,
            "active_owner",
            None,
            Some(&authorization),
        )
        .unwrap();
        assert_eq!(target.name, expected);
        assert_eq!(target.role_owner, expected);
    }
    let error = schema_creation_target(
        &catalog,
        &catalog,
        "active_owner",
        Some("existing"),
        Some(&SchemaAuthorization::Role("absent".into())),
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
}

#[test]
fn schema_reserved_prefix_keeps_postgresql_case_sensitive_identifier_rules() {
    for name in ["pg_private", "pg_catalog", "pg_temp_42"] {
        let error = validate_schema_creation_name(name).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42939"));
    }
    validate_schema_creation_name("PG_private").unwrap();
    validate_schema_creation_name("application").unwrap();
}
