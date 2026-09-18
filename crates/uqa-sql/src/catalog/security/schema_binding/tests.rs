//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::schema::{role_has_schema_privilege, SchemaAclPrivilege};

fn roles() -> BTreeMap<String, RoleDefinition> {
    let owner = RoleDefinition::bootstrap();
    let mut reader = owner.clone();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    BTreeMap::from([(owner.name.clone(), owner), (reader.name.clone(), reader)])
}

#[test]
fn schema_identity_projection_preserves_original_authority_and_rejects_replacements() {
    let mut roles = roles();
    let security = SchemaSecurity {
        role_owner: "uqa".into(),
        acl: Some(vec![SchemaAclEntry {
            role: "reader".into(),
            grantor: None,
            privileges: SchemaPrivileges::ALL,
            grant_options: SchemaPrivileges::default(),
        }]),
    };
    let bound = BoundSchemaSecurity::bind(&security, &roles).unwrap();
    let reader = roles["reader"].identity();
    let mut renamed = roles.remove("reader").unwrap();
    renamed.name = "renamed".into();
    let mut replacement = renamed.clone();
    replacement.name = "reader".into();
    replacement.oid += 1;
    replacement.object_id = [2; 16];
    roles.insert(renamed.name.clone(), renamed);
    roles.insert(replacement.name.clone(), replacement);
    let names = bound.resolve(&roles).unwrap();
    assert!(role_has_schema_privilege(
        &names,
        "renamed",
        SchemaAclPrivilege::Create,
        &roles,
        &BTreeMap::new()
    ));
    assert!(!role_has_schema_privilege(
        &names,
        "reader",
        SchemaAclPrivilege::Create,
        &roles,
        &BTreeMap::new()
    ));
    assert!(bound.depends_on(reader));
    assert!(!bound.depends_on(roles["reader"].identity()));
    assert_eq!(BoundSchemaSecurity::bind(&names, &roles).unwrap(), bound);
    roles.get_mut("renamed").unwrap().object_id = [3; 16];
    assert!(bound.validate(&roles).is_err());
    assert!(bound.resolve(&roles).is_err());
}

#[test]
fn schema_defaults_and_public_access_retain_the_bootstrap_incarnation() {
    let mut roles = roles();
    let mut owner = roles.remove("uqa").unwrap();
    owner.name = "renamed_owner".into();
    roles.insert(owner.name.clone(), owner);
    let bound = BoundSchemaSecurity::bootstrap("public");
    let security = bound.resolve(&roles).unwrap();
    assert_eq!(security.role_owner, "renamed_owner");
    assert!(role_has_schema_privilege(
        &security,
        "reader",
        SchemaAclPrivilege::Usage,
        &roles,
        &BTreeMap::new()
    ));
    assert!(!role_has_schema_privilege(
        &security,
        "reader",
        SchemaAclPrivilege::Create,
        &roles,
        &BTreeMap::new()
    ));
    let ordinary = BoundSchemaSecurity::bootstrap("ordinary");
    assert!(ordinary.acl.is_none());
    assert_eq!(
        BoundSchemaSecurity::from_row(ordinary.row("ordinary")),
        ("ordinary".into(), ordinary)
    );
    roles.get_mut("renamed_owner").unwrap().object_id = [9; 16];
    assert!(bound.validate(&roles).is_err());
}
