//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Statement, TablePrivilegeSpec};
use crate::catalog::{
    resolution::RelationResolution,
    security::{
        columns::role_has_column_privilege,
        grants::{bind_grant_schemas, GrantNamespace},
        table::{requested_acl_privileges, role_has_privilege, TablePrivilegeCheck},
    },
};
use std::cell::RefCell;

fn statement(sql: &str) -> GrantTableStmt {
    let Statement::GrantTable(statement) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected table grant")
    };
    statement
}
fn roles() -> BTreeMap<String, RoleDefinition> {
    let mut roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for name in ["alice", "reader", "independent"] {
        let Statement::CreateRole(role) = crate::compile(&format!("CREATE ROLE {name}"))
            .unwrap()
            .remove(0)
        else {
            panic!("expected role")
        };
        roles.insert(name.into(), RoleDefinition::from_create(&role));
    }
    roles
}
type AppliedGrant = (TableSecurity, usize, Vec<(&'static str, String)>);

fn apply(
    sql: &str,
    user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &TableSecurity,
) -> Result<AppliedGrant, SQLError> {
    let statement = statement(sql);
    let requested = requested_acl_privileges(&statement.privileges)?;
    let memberships = BTreeMap::new();
    let application = TableGrantApplication {
        statement: &statement,
        grantees: &statement.grantees,
        requested: &requested,
        current_user: user,
        roles,
        memberships: &memberships,
    };
    let (next, grantable) = application.apply(current)?;
    let mut notices = Vec::new();
    application.record_warning(
        grantable,
        &RelationIdentity::new("public", "items"),
        &mut notices,
    );
    Ok((next, grantable, notices))
}
fn column_allowed(
    security: &TableSecurity,
    column: &str,
    role: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> bool {
    role_has_column_privilege(
        security,
        column,
        role,
        TablePrivilegeCheck {
            privilege: TableAclPrivilege::Update,
            grant_option: false,
        },
        roles,
        &BTreeMap::new(),
    )
}
fn target(kind: &'static str) -> ResolvedTableGrantTarget {
    ResolvedTableGrantTarget {
        requested: "items".into(),
        name: "public.items".into(),
        relation: RelationIdentity::new("public", "items"),
        kind,
    }
}

#[test]
fn table_and_column_grants_preserve_input_security_and_grant_options() {
    let roles = roles();
    let current = TableSecurity::owner("uqa");
    let (next, count, notices) = apply(
        "GRANT SELECT, UPDATE(id) ON TABLE items TO reader WITH GRANT OPTION",
        "uqa",
        &roles,
        &current,
    )
    .unwrap();
    assert_eq!(count, 2);
    assert!(notices.is_empty());
    assert_eq!(current, TableSecurity::owner("uqa"));
    assert!(role_has_privilege(
        &next,
        "reader",
        TablePrivilegeCheck {
            privilege: TableAclPrivilege::Select,
            grant_option: true
        },
        &roles,
        &BTreeMap::new()
    ));
    assert!(role_has_column_privilege(
        &next,
        "id",
        "reader",
        TablePrivilegeCheck {
            privilege: TableAclPrivilege::Update,
            grant_option: true
        },
        &roles,
        &BTreeMap::new()
    ));
    assert!(!column_allowed(&next, "other", "reader", &roles));
}

#[test]
fn column_revoke_restrict_preserves_input_and_cascade_keeps_independent_grant_paths() {
    let roles = roles();
    let security = TableSecurity::owner("uqa");
    let (security, _, _) = apply(
        "GRANT UPDATE(id) ON items TO alice WITH GRANT OPTION",
        "uqa",
        &roles,
        &security,
    )
    .unwrap();
    let (security, _, _) = apply(
        "GRANT UPDATE(id) ON items TO reader",
        "alice",
        &roles,
        &security,
    )
    .unwrap();
    let (security, _, _) = apply(
        "GRANT UPDATE(id) ON items TO independent",
        "uqa",
        &roles,
        &security,
    )
    .unwrap();
    let before = security.clone();
    assert_eq!(
        apply(
            "REVOKE UPDATE(id) ON items FROM alice RESTRICT",
            "uqa",
            &roles,
            &security
        )
        .unwrap_err()
        .sqlstate(),
        Some("2BP01")
    );
    assert_eq!(security, before);
    let (next, _, _) = apply(
        "REVOKE UPDATE(id) ON items FROM alice CASCADE",
        "uqa",
        &roles,
        &security,
    )
    .unwrap();
    assert!(!column_allowed(&next, "id", "alice", &roles));
    assert!(!column_allowed(&next, "id", "reader", &roles));
    assert!(column_allowed(&next, "id", "independent", &roles));
}

#[test]
fn partial_grant_publishes_only_authorized_privileges_and_reports_a_warning() {
    let roles = roles();
    let (current, _, _) = apply(
        "GRANT SELECT ON items TO alice WITH GRANT OPTION",
        "uqa",
        &roles,
        &TableSecurity::owner("uqa"),
    )
    .unwrap();
    let (next, count, notices) = apply(
        "GRANT SELECT, UPDATE ON items TO reader",
        "alice",
        &roles,
        &current,
    )
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        notices,
        vec![(
            "WARNING",
            "not all privileges were granted for \"items\"".into()
        )]
    );
    assert!(role_has_privilege(
        &next,
        "reader",
        TablePrivilegeCheck {
            privilege: TableAclPrivilege::Select,
            grant_option: false
        },
        &roles,
        &BTreeMap::new()
    ));
    assert!(!column_allowed(&next, "id", "reader", &roles));
}

#[test]
fn absent_grant_authority_preserves_security_and_distinguishes_grant_and_revoke_warnings() {
    let roles = roles();
    let current = TableSecurity::owner("uqa");
    for (sql, message) in [
        (
            "GRANT SELECT ON items TO reader",
            "no privileges were granted for \"items\"",
        ),
        (
            "REVOKE SELECT ON items FROM reader",
            "no privileges could be revoked for \"items\"",
        ),
    ] {
        let (next, count, notices) = apply(sql, "alice", &roles, &current).unwrap();
        assert_eq!(next, current);
        assert_eq!(count, 0);
        assert_eq!(notices, vec![("WARNING", message.into())]);
    }
}

#[test]
fn role_errors_precede_public_grant_options_and_explicit_grantor_validation() {
    let roles = roles();
    let mut grant = statement("GRANT SELECT ON items TO PUBLIC WITH GRANT OPTION");
    assert_eq!(
        validate_table_acl_roles(
            &grant,
            &["absent".into(), "PUBLIC".into()],
            Some("absent"),
            "uqa",
            &roles
        )
        .unwrap_err()
        .sqlstate(),
        Some("42704")
    );
    assert_eq!(
        validate_table_acl_roles(&grant, &["PUBLIC".into()], Some("absent"), "uqa", &roles)
            .unwrap_err()
            .sqlstate(),
        Some("0LP01")
    );
    grant.grant_option = false;
    assert_eq!(
        validate_table_acl_roles(&grant, &["reader".into()], Some("absent"), "uqa", &roles)
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    assert_eq!(
        validate_table_acl_roles(&grant, &["reader".into()], Some("alice"), "uqa", &roles)
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    validate_table_acl_roles(&grant, &["PUBLIC".into()], Some("uqa"), "uqa", &roles).unwrap();
}

#[test]
fn sequence_targets_reject_column_privileges_before_acl_application() {
    let grant = statement("GRANT SELECT(id) ON items TO reader");
    let error = validate_table_grant_target_kinds(&grant, &[target("table"), target("sequence")])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert!(error
        .to_string()
        .contains("column \"id\" of relation \"items\" does not exist"));
    validate_table_grant_target_kinds(
        &statement("GRANT SELECT ON items TO reader"),
        &[target("foreign table")],
    )
    .unwrap();
}

#[test]
fn table_syntax_sequence_privileges_keep_input_order_and_mark_inapplicable_permissions() {
    assert_eq!(
        table_sequence_privileges(&[]),
        (
            vec![
                SequencePrivilege::Select,
                SequencePrivilege::Update,
                SequencePrivilege::Usage
            ],
            false
        )
    );
    let specs = vec![
        TablePrivilegeSpec {
            privilege: TablePrivilege::Update,
            columns: Vec::new(),
        },
        TablePrivilegeSpec {
            privilege: TablePrivilege::Insert,
            columns: Vec::new(),
        },
        TablePrivilegeSpec {
            privilege: TablePrivilege::Select,
            columns: Vec::new(),
        },
        TablePrivilegeSpec {
            privilege: TablePrivilege::Update,
            columns: Vec::new(),
        },
    ];
    assert_eq!(
        table_sequence_privileges(&specs),
        (
            vec![SequencePrivilege::Update, SequencePrivilege::Select],
            true
        )
    );
}

struct Resolution {
    outcome: RelationResolution,
    calls: RefCell<Vec<String>>,
}
impl targets::TableGrantResolution for Resolution {
    fn resolve_visible_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.calls.borrow_mut().push(name.into());
        Ok(if name == "present" {
            RelationResolution::Found("public.present".into(), "table")
        } else {
            self.outcome.clone()
        })
    }
}

#[test]
fn named_targets_stop_at_the_first_missing_schema_or_relation() {
    for (outcome, state) in [
        (RelationResolution::MissingSchema("absent".into()), "3F000"),
        (RelationResolution::MissingRelation, "42P01"),
    ] {
        let resolution = Resolution {
            outcome,
            calls: RefCell::new(Vec::new()),
        };
        let error = targets::bind_named_table_grants(
            &resolution,
            &["present".into(), "missing".into(), "never".into()],
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(*resolution.calls.borrow(), vec!["present", "missing"]);
    }
}

struct Namespace {
    allocated: bool,
}
impl GrantNamespace for Namespace {
    fn temporary_schema_name(&self) -> String {
        "pg_temp_42".into()
    }
    fn temporary_namespace_allocated(&self) -> bool {
        self.allocated
    }
    fn has_namespace(&self, name: &str) -> Result<bool, String> {
        Ok(name == "public")
    }
}

#[test]
fn schema_grants_share_temporary_alias_binding_and_preserve_first_occurrence_order() {
    assert_eq!(
        bind_grant_schemas(
            &Namespace { allocated: true },
            &[
                "public".into(),
                "pg_temp".into(),
                "public".into(),
                "pg_temp_42".into()
            ]
        )
        .unwrap(),
        vec!["public", "pg_temp_42"]
    );
    let error =
        bind_grant_schemas(&Namespace { allocated: false }, &["pg_temp".into()]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("3F000"));
    assert!(error
        .to_string()
        .contains("schema \"pg_temp\" does not exist"));
}

#[test]
fn foreign_acl_candidates_validate_columns_without_mutating_the_source_security() {
    let roles = roles();
    let statement = statement("GRANT UPDATE(id) ON items TO reader");
    let requested = requested_acl_privileges(&statement.privileges).unwrap();
    let memberships = BTreeMap::new();
    let application = TableGrantApplication {
        statement: &statement,
        grantees: &statement.grantees,
        requested: &requested,
        current_user: "uqa",
        roles: &roles,
        memberships: &memberships,
    };
    let target = target("foreign table");
    let security = TableSecurity::owner("uqa");
    let updates = foreign_table_privilege_updates(
        vec![(&target, security.clone(), vec!["id".into()])],
        &application,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(updates.len(), 1);
    assert!(column_allowed(&updates[0].1, "id", "reader", &roles));
    assert_eq!(security, TableSecurity::owner("uqa"));
    assert!(foreign_table_privilege_updates(
        vec![(&target, security, vec!["different".into()])],
        &application,
        &mut Vec::new()
    )
    .is_err());
}
