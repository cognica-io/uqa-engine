//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{columns, sequence, table, SequenceSecurity, TableSecurity};
use crate::{
    ast::RoleAttribute,
    catalog::roles::{identity::RoleBinding, RoleDefinition, RoleMembership, RoleMembershipKey},
};
use std::collections::{BTreeMap, BTreeSet};

fn authority(
    inherit: bool,
) -> (
    BTreeMap<String, RoleDefinition>,
    BTreeMap<RoleMembershipKey, RoleMembership>,
) {
    let mut roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for (index, name) in ["owner", "member", "outsider"].into_iter().enumerate() {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_000 + index as i64;
        role.object_id = [index as u8 + 1; 16];
        role.attributes = BTreeSet::from([RoleAttribute::Inherit]);
        roles.insert(name.into(), role);
    }
    let membership = RoleMembership {
        oid: 30_000,
        role: RoleBinding::from_definition(&roles["owner"]).unwrap(),
        member: RoleBinding::from_definition(&roles["member"]).unwrap(),
        grantor: RoleBinding::from_definition(&roles["uqa"]).unwrap(),
        admin_option: false,
        inherit_option: inherit,
        set_option: true,
    };
    (roles, BTreeMap::from([(membership.key(), membership)]))
}

#[test]
fn table_owners_keep_grant_options_while_ordinary_privileges_follow_the_acl() {
    for inherit in [false, true] {
        let (roles, memberships) = authority(inherit);
        for acl in [None, Some(Vec::new())] {
            let security = TableSecurity {
                acl,
                ..TableSecurity::owner("owner")
            };
            for subject in ["owner", "member", "outsider", "uqa"] {
                let owns = subject == "owner" || (subject == "member" && inherit);
                for privilege in table::TableAclPrivilege::ALL {
                    for grant_option in [false, true] {
                        let expected =
                            subject == "uqa" || (owns && (grant_option || security.acl.is_none()));
                        assert_eq!(
                            table::role_has_privilege(
                                &security,
                                subject,
                                table::TablePrivilegeCheck {
                                    privilege,
                                    grant_option
                                },
                                &roles,
                                &memberships
                            ),
                            expected,
                            "{subject}: {privilege:?}: grant={grant_option}: acl={:?}",
                            security.acl
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sequence_owners_keep_metadata_and_grant_options_after_ordinary_revocation() {
    for inherit in [false, true] {
        let (roles, memberships) = authority(inherit);
        for acl in [None, Some(Vec::new())] {
            let security = SequenceSecurity {
                role_owner: "owner".into(),
                acl,
            };
            for subject in ["owner", "member", "outsider", "uqa"] {
                let owns = subject == "owner" || (subject == "member" && inherit);
                for privilege in [
                    sequence::AclPrivilege::Select,
                    sequence::AclPrivilege::Usage,
                    sequence::AclPrivilege::Update,
                ] {
                    for grant_option in [false, true] {
                        let expected =
                            subject == "uqa" || (owns && (grant_option || security.acl.is_none()));
                        assert_eq!(
                            sequence::role_has_privilege(
                                &security,
                                subject,
                                sequence::PrivilegeCheck {
                                    privilege,
                                    grant_option
                                },
                                &roles,
                                &memberships
                            ),
                            expected,
                            "{subject}: {privilege:?}: grant={grant_option}: acl={:?}",
                            security.acl
                        );
                    }
                }
                assert_eq!(
                    sequence::role_can_view_sequence(&security, subject, &roles, &memberships),
                    owns || subject == "uqa"
                );
                assert_eq!(
                    sequence::role_has_any_sequence_privilege(
                        &security,
                        subject,
                        &roles,
                        &memberships
                    ),
                    subject == "uqa" || (owns && security.acl.is_none())
                );
            }
        }
    }
}

#[test]
fn column_grants_do_not_restore_revoked_table_or_other_column_privileges() {
    let (roles, memberships) = authority(true);
    let mut security = TableSecurity {
        acl: Some(Vec::new()),
        ..TableSecurity::owner("owner")
    };
    columns::grant_column_acl(
        &mut security,
        "allowed",
        table::TableAclPrivilege::Select,
        &["owner".into()],
        "owner",
        false,
    );
    for subject in ["owner", "member"] {
        for column in ["allowed", "other"] {
            for privilege in table::TableAclPrivilege::COLUMN_ALL {
                for grant_option in [false, true] {
                    let expected = grant_option
                        || (column == "allowed" && privilege == table::TableAclPrivilege::Select);
                    assert_eq!(
                        columns::role_has_column_privilege(
                            &security,
                            column,
                            subject,
                            table::TablePrivilegeCheck {
                                privilege,
                                grant_option
                            },
                            &roles,
                            &memberships
                        ),
                        expected
                    );
                }
            }
        }
        assert!(!table::role_has_table_privilege(
            &security,
            subject,
            table::TableAclPrivilege::Select,
            &roles,
            &memberships
        ));
    }
}
