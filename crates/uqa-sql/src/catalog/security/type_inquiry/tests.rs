//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::EnumTypeReference;
use crate::catalog::roles::identity::RoleBinding;

const FEELING: u32 = 20_000;

fn role(name: &str, oid: i64, attributes: &[RoleAttribute]) -> RoleDefinition {
    RoleDefinition {
        name: name.into(),
        oid,
        object_id: [u8::try_from(oid - 10_000).unwrap(); 16],
        revision: 1,
        attributes: attributes.iter().copied().collect(),
        connection_limit: -1,
    }
}

fn roles() -> BTreeMap<String, RoleDefinition> {
    [
        role("owner", 10_001, &[]),
        role("user", 10_002, &[]),
        role("group", 10_003, &[]),
        role("admin", 10_004, &[RoleAttribute::Superuser]),
    ]
    .into_iter()
    .map(|role| (role.name.clone(), role))
    .collect()
}

struct Types {
    owner: RoleIdentity,
    acl: Option<Vec<ObjectAclEntry>>,
}

impl TypePrivilegeCatalog for Types {
    fn resolve_type_name(&self, name: &str) -> Result<u32, SQLError> {
        Err(SQLError::Internal(format!("unexpected type name {name}")))
    }

    fn type_privileges(&self, oid: u32) -> Option<TypePrivileges<'_>> {
        (oid == FEELING).then_some(TypePrivileges {
            owner: self.owner,
            usage_acl: self.acl.as_deref(),
        })
    }
}

fn usage(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    types: &Types,
    user: &str,
) -> Result<(), SQLError> {
    TypePrivilegeInquiry {
        current_user: &RoleReference::from(user),
        roles,
        memberships,
        catalog: types,
    }
    .require_usage(FEELING, || Ok("other.feeling".into()))
}

fn entry(role: Option<RoleIdentity>, grantor: RoleIdentity) -> ObjectAclEntry {
    ObjectAclEntry {
        role,
        grantor,
        grant_option: false,
    }
}

#[test]
fn usage_is_governed_by_the_array_element_or_the_domain_itself() {
    let feeling = ColumnType::Enum(EnumTypeReference {
        schema: "other".into(),
        name: "feeling".into(),
        oid: FEELING,
        array_oid: FEELING + 1,
    });
    let nested = ColumnType::Array(Box::new(ColumnType::Array(Box::new(feeling.clone()))));
    assert_eq!(
        usage_governing_type(&nested),
        Some((FEELING, &feeling)),
        "an array defers to its element type"
    );
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "good_mood".into(),
        oid: 30_000,
        array_oid: None,
        base: Box::new(feeling),
    };
    assert_eq!(
        usage_governing_type(&domain).map(|(oid, _)| oid),
        Some(30_000),
        "a domain governs itself, not its base type"
    );
    assert_eq!(
        usage_governing_type(&ColumnType::Array(Box::new(ColumnType::Integer))),
        None
    );
}

#[test]
fn usage_follows_the_acl_ownership_membership_and_superuser() {
    let roles = roles();
    let owner = roles["owner"].identity();
    let mut memberships = BTreeMap::new();
    let mut types = Types { owner, acl: None };
    // The default privileges grant USAGE to PUBLIC.
    usage(&roles, &memberships, &types, "user").unwrap();

    // REVOKE USAGE ... FROM PUBLIC leaves only the owner's entry.
    types.acl = Some(vec![entry(Some(owner), owner)]);
    usage(&roles, &memberships, &types, "owner").unwrap();
    usage(&roles, &memberships, &types, "admin").unwrap();
    let error = usage(&roles, &memberships, &types, "user").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(
        error.to_string(),
        "permission denied for type other.feeling"
    );

    // A grant to a role the user inherits reaches the user.
    types
        .acl
        .as_mut()
        .unwrap()
        .push(entry(Some(roles["group"].identity()), owner));
    let membership = RoleMembership {
        oid: 1,
        role: RoleBinding::from_definition(&roles["group"]).unwrap(),
        member: RoleBinding::from_definition(&roles["user"]).unwrap(),
        grantor: RoleBinding::from_definition(&roles["owner"]).unwrap(),
        admin_option: false,
        inherit_option: true,
        set_option: true,
    };
    memberships.insert(membership.key(), membership);
    usage(&roles, &memberships, &types, "user").unwrap();

    // A membership without INHERIT does not.
    for membership in memberships.values_mut() {
        membership.inherit_option = false;
    }
    assert!(usage(&roles, &memberships, &types, "user").is_err());
}

#[test]
fn a_granted_usage_does_not_spell_the_type() {
    let roles = roles();
    let types = Types {
        owner: roles["owner"].identity(),
        acl: None,
    };
    TypePrivilegeInquiry {
        current_user: &RoleReference::from("user"),
        roles: &roles,
        memberships: &BTreeMap::new(),
        catalog: &types,
    }
    .require_usage(FEELING, || unreachable!("only a denial names the type"))
    .unwrap();
}
