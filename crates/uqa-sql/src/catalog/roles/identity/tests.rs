//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RoleAttribute,
    catalog::{
        roles::{memberships::role_is_superuser, role_can_set, role_inherits, RoleMembership},
        security::{
            columns, database, schema, sequence, table, SchemaSecurity, SequenceSecurity,
            TableSecurity,
        },
    },
};

fn actor() -> RoleDefinition {
    RoleDefinition {
        name: "actor".into(),
        oid: 16_384,
        object_id: [1; 16],
        attributes: [RoleAttribute::Inherit].into(),
        connection_limit: -1,
    }
}

fn membership_catalog() -> (
    BTreeMap<String, RoleDefinition>,
    BTreeMap<super::super::RoleMembershipKey, RoleMembership>,
) {
    let member = actor();
    let mut group = member.clone();
    group.name = "group".into();
    group.oid = 16_385;
    group.object_id = [2; 16];
    let bootstrap = RoleDefinition::bootstrap();
    let membership = RoleMembership {
        oid: 16_386,
        role: RoleBinding::from_definition(&group).unwrap(),
        member: RoleBinding::from_definition(&member).unwrap(),
        grantor: RoleBinding::from_definition(&bootstrap).unwrap(),
        admin_option: true,
        inherit_option: true,
        set_option: true,
    };
    let roles = [member, group, bootstrap]
        .into_iter()
        .map(|role| (role.name.clone(), role))
        .collect();
    (roles, BTreeMap::from([(membership.key(), membership)]))
}

#[test]
fn selected_role_follows_attributes_and_rename_but_not_name_or_oid_reuse() {
    let original = actor();
    let selected = RoleBinding::from_definition(&original).unwrap();
    let mut roles = BTreeMap::from([(original.name.clone(), original.clone())]);
    assert_eq!(selected.require_name(&roles).unwrap(), "actor");
    assert!(!role_is_superuser(&roles, &selected));
    roles
        .get_mut("actor")
        .unwrap()
        .attributes
        .insert(RoleAttribute::Superuser);
    assert!(role_is_superuser(&roles, &selected));
    let mut renamed = roles.remove("actor").unwrap();
    renamed.name = "renamed".into();
    roles.insert(renamed.name.clone(), renamed);
    assert_eq!(selected.require_name(&roles).unwrap(), "renamed");
    assert_eq!(
        selected.revalidate(&roles).unwrap_err().sqlstate(),
        Some("42704")
    );
    assert!(role_is_superuser(&roles, &selected));

    for (oid, object_id) in [(16_385, [1; 16]), (16_384, [2; 16]), (16_385, [2; 16])] {
        let mut replacement = original.clone();
        replacement.oid = oid;
        replacement.object_id = object_id;
        replacement.attributes.insert(RoleAttribute::Superuser);
        let roles = BTreeMap::from([(replacement.name.clone(), replacement)]);
        assert!(role_is_superuser(&roles, "actor"));
        assert!(!role_is_superuser(&roles, &selected));
        assert_eq!(
            selected.require_name(&roles).unwrap_err().sqlstate(),
            Some("42704")
        );
        assert_eq!(
            selected.revalidate(&roles).unwrap_err().sqlstate(),
            Some("42704")
        );
    }
}

#[test]
fn reused_original_name_does_not_override_a_renamed_role_identity() {
    let original = actor();
    let selected = RoleBinding::from_definition(&original).unwrap();
    let mut renamed = original.clone();
    renamed.name = "renamed".into();
    let mut replacement = original;
    replacement.oid += 1;
    replacement.object_id = [2; 16];
    replacement.attributes.insert(RoleAttribute::Superuser);
    let mut roles = BTreeMap::from([
        (renamed.name.clone(), renamed),
        (replacement.name.clone(), replacement),
    ]);
    assert_eq!(selected.require_name(&roles).unwrap(), "renamed");
    assert!(!role_is_superuser(&roles, &selected));
    assert!(role_is_superuser(&roles, "actor"));
    assert_eq!(
        selected.revalidate(&roles).unwrap_err().sqlstate(),
        Some("42704")
    );
    roles.remove("renamed");
    assert_eq!(
        selected.require_name(&roles).unwrap_err().sqlstate(),
        Some("42704")
    );
    assert!(!role_is_superuser(&roles, &selected));
}

#[test]
fn deleted_role_cannot_inherit_or_set_replacement_memberships() {
    let (mut roles, mut memberships) = membership_catalog();
    let selected = RoleBinding::from_definition(&roles["actor"]).unwrap();
    assert!(role_inherits(&roles, &memberships, &selected, "group"));
    assert!(role_can_set(&roles, &memberships, &selected, "group"));
    roles.get_mut("actor").unwrap().object_id = [3; 16];
    let (_, mut replacement_membership) = memberships.pop_first().unwrap();
    replacement_membership.member = RoleBinding::from_definition(&roles["actor"]).unwrap();
    memberships.insert(replacement_membership.key(), replacement_membership);
    for target in ["actor", "group"] {
        assert!(!role_inherits(&roles, &memberships, &selected, target));
        assert!(!role_can_set(&roles, &memberships, &selected, target));
        assert!(role_inherits(&roles, &memberships, "actor", target));
        assert!(role_can_set(&roles, &memberships, "actor", target));
    }
}

#[test]
fn retained_memberships_never_authorize_recreated_endpoints_even_when_oids_match() {
    use crate::catalog::roles::memberships::{pg_has_role_privilege, RolePrivilegeCheck};
    for endpoint in ["actor", "group"] {
        for reuse_oid in [false, true] {
            let (mut roles, memberships) = membership_catalog();
            let replacement = roles.get_mut(endpoint).unwrap();
            replacement.object_id = [3; 16];
            if !reuse_oid {
                replacement.oid += 100;
            }
            assert!(!role_inherits(&roles, &memberships, "actor", "group"));
            assert!(!role_can_set(&roles, &memberships, "actor", "group"));
            for privilege in [RolePrivilegeCheck::Member, RolePrivilegeCheck::Admin] {
                assert!(!pg_has_role_privilege(
                    &roles,
                    &memberships,
                    Some("actor"),
                    Some("group"),
                    privilege
                ));
            }
        }
    }
}

#[test]
fn membership_keys_and_authority_follow_role_identity_across_name_changes() {
    let (mut roles, memberships) = membership_catalog();
    let original = memberships.values().next().unwrap();
    let mut renamed = roles.remove("group").unwrap();
    renamed.name = "renamed_group".into();
    let mut same_membership = original.clone();
    same_membership.role = RoleBinding::from_definition(&renamed).unwrap();
    roles.insert(renamed.name.clone(), renamed);
    assert_eq!(original.key(), same_membership.key());
    assert!(role_inherits(
        &roles,
        &memberships,
        "actor",
        "renamed_group"
    ));
    assert!(role_can_set(&roles, &memberships, "actor", "renamed_group"));
    let mut replacement = roles["renamed_group"].clone();
    replacement.name = "group".into();
    replacement.oid += 100;
    replacement.object_id = [3; 16];
    roles.insert(replacement.name.clone(), replacement);
    assert!(!role_inherits(&roles, &memberships, "actor", "group"));
}

struct Permissions {
    relation: TableSecurity,
    column: TableSecurity,
    namespace: SchemaSecurity,
    counter: SequenceSecurity,
    database: database::DatabaseSecurity,
}

impl Permissions {
    fn new(grantee: &str) -> Self {
        let grantees = [grantee.to_string()];
        let grant = grantee != "PUBLIC";
        let mut relation = TableSecurity::owner("uqa");
        let mut column = TableSecurity::owner("uqa");
        let mut namespace = SchemaSecurity {
            role_owner: "uqa".into(),
            acl: Some(Vec::new()),
        };
        let mut counter = SequenceSecurity {
            role_owner: "uqa".into(),
            acl: Some(Vec::new()),
        };
        let mut database = database::DatabaseSecurity {
            role_owner: "uqa".into(),
            acl: Some(Vec::new()),
        };
        table::grant_acl(
            &mut relation,
            table::TableAclPrivilege::Select,
            &grantees,
            "uqa",
            grant,
        );
        columns::grant_column_acl(
            &mut column,
            "v",
            table::TableAclPrivilege::Select,
            &grantees,
            "uqa",
            grant,
        );
        schema::grant_acl(
            &mut namespace,
            schema::SchemaAclPrivilege::Usage,
            &grantees,
            "uqa",
            grant,
        );
        sequence::grant_acl(
            &mut counter,
            sequence::AclPrivilege::Usage,
            &grantees,
            "uqa",
            grant,
        );
        database::grant_acl(
            &mut database,
            database::DatabaseAclPrivilege::Connect,
            &grantees,
            "uqa",
            grant,
        );
        Self {
            relation,
            column,
            namespace,
            counter,
            database,
        }
    }

    fn assert_privileges(
        &self,
        selected: &RoleBinding,
        roles: &BTreeMap<String, RoleDefinition>,
        grant_option: bool,
        expected: bool,
    ) {
        let memberships = BTreeMap::new();
        let check = table::TablePrivilegeCheck {
            privilege: table::TableAclPrivilege::Select,
            grant_option,
        };
        assert_eq!(
            table::role_has_privilege(&self.relation, selected, check, roles, &memberships),
            expected
        );
        assert_eq!(
            columns::role_has_column_privilege(
                &self.column,
                "v",
                selected,
                check,
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            schema::role_has_schema_privilege_check(
                &self.namespace,
                selected,
                schema::SchemaPrivilegeCheck {
                    privilege: schema::SchemaAclPrivilege::Usage,
                    grant_option
                },
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            sequence::role_has_privilege(
                &self.counter,
                selected,
                sequence::PrivilegeCheck {
                    privilege: sequence::AclPrivilege::Usage,
                    grant_option
                },
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            database::role_has_database_privilege_check(
                &self.database,
                selected,
                database::DatabasePrivilegeCheck {
                    privilege: database::DatabaseAclPrivilege::Connect,
                    grant_option
                },
                roles,
                &memberships
            ),
            expected
        );
    }

    fn assert_grantor(
        &self,
        selected: &RoleBinding,
        roles: &BTreeMap<String, RoleDefinition>,
        expected: Option<String>,
    ) {
        let memberships = BTreeMap::new();
        assert_eq!(
            table::select_acl_grantor(
                &self.relation,
                table::TableAclPrivilege::Select,
                selected,
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            columns::select_column_acl_grantor(
                &self.column,
                "v",
                table::TableAclPrivilege::Select,
                selected,
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            schema::select_acl_grantor(
                &self.namespace,
                schema::SchemaAclPrivilege::Usage,
                selected,
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            sequence::select_acl_grantor(
                &self.counter,
                sequence::AclPrivilege::Usage,
                selected,
                roles,
                &memberships
            ),
            expected
        );
        assert_eq!(
            database::select_acl_grantor(
                &self.database,
                database::DatabaseAclPrivilege::Connect,
                selected,
                roles,
                &memberships
            ),
            expected
        );
    }
}

#[test]
fn deleted_role_retains_public_access_without_replacement_acl_or_grant_options() {
    let original = actor();
    let selected = RoleBinding::from_definition(&original).unwrap();
    let mut replacement = original.clone();
    replacement.object_id = [2; 16];
    for grantee in ["actor", "PUBLIC"] {
        let permissions = Permissions::new(grantee);
        for role in [Some(original.clone()), None, Some(replacement.clone())] {
            let live = role
                .as_ref()
                .is_some_and(|role| role.object_id == original.object_id);
            let roles = role
                .into_iter()
                .map(|role| (role.name.clone(), role))
                .collect();
            let grant = live && grantee != "PUBLIC";
            permissions.assert_privileges(&selected, &roles, false, live || grantee == "PUBLIC");
            permissions.assert_privileges(&selected, &roles, true, grant);
            permissions.assert_grantor(&selected, &roles, grant.then(|| "actor".to_string()));
        }
    }
}
