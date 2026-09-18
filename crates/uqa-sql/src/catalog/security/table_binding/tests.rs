//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::{
    columns::grant_column_acl,
    table::{grant_acl, role_has_column_privilege, TableAclPrivilege},
};

fn roles() -> BTreeMap<String, RoleDefinition> {
    ["owner", "grantor", "reader"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_001 + index as i64;
            role.object_id = [index as u8 + 1; 16];
            role.attributes.clear();
            (name.into(), role)
        })
        .collect()
}

fn security(roles: &BTreeMap<String, RoleDefinition>) -> BoundTableSecurity {
    let mut named = TableSecurity::owner("owner");
    grant_acl(
        &mut named,
        TableAclPrivilege::Select,
        &["grantor".into()],
        "owner",
        true,
    );
    grant_column_acl(
        &mut named,
        "id",
        TableAclPrivilege::Select,
        &["reader".into()],
        "grantor",
        false,
    );
    BoundTableSecurity::bind(&named, roles).unwrap()
}

#[test]
fn table_and_column_paths_keep_incarnations_when_all_endpoint_names_change() {
    let mut roles = roles();
    let bound = security(&roles);
    let original = bound.clone();
    let owner = bound.owner_reference(&roles).unwrap();
    for name in ["owner", "grantor", "reader"] {
        let mut role = roles.remove(name).unwrap();
        let mut replacement = role.clone();
        replacement.oid += 100;
        replacement.object_id[0] += 100;
        roles.insert(name.into(), replacement);
        role.name = format!("renamed_{name}");
        roles.insert(role.name.clone(), role);
    }
    bound.validate(Some(&["id".into()]), &roles).unwrap();
    let projected = bound.resolve(&roles).unwrap();
    assert_eq!(projected.role_owner, "renamed_owner");
    assert_eq!(owner.require_name(&roles).unwrap(), "renamed_owner");
    assert_eq!(
        projected.column_acls["id"][0].grantor.as_deref(),
        Some("renamed_grantor")
    );
    for (name, expected) in [("renamed_reader", true), ("reader", false)] {
        assert_eq!(
            role_has_column_privilege(
                &projected,
                "id",
                name,
                TableAclPrivilege::Select,
                &roles,
                &BTreeMap::new()
            ),
            expected
        );
    }
    assert!(bound.depends_on(roles["renamed_reader"].identity()));
    assert!(!bound.depends_on(roles["reader"].identity()));
    assert_eq!(bound, original);
}

#[test]
fn missing_or_reused_role_incarnations_cannot_rebind_any_table_acl_endpoint() {
    for name in ["owner", "grantor", "reader"] {
        let mut roles = roles();
        let bound = security(&roles);
        let mut replacement = roles.remove(name).unwrap();
        assert!(bound.resolve(&roles).is_err());
        replacement.object_id[0] += 100;
        roles.insert(name.into(), replacement);
        assert!(bound.resolve(&roles).is_err());
        assert!(bound.validate(Some(&["id".into()]), &roles).is_err());
    }
}

#[test]
fn bound_table_validation_preserves_column_and_grant_path_rules() {
    let roles = roles();
    let mut bound = security(&roles);
    assert!(bound.validate(None, &roles).is_err());
    assert!(bound.validate(Some(&["other".into()]), &roles).is_err());
    let column = &mut bound.column_acls.get_mut("id").unwrap()[0];
    column.role = None;
    bound.validate(Some(&["id".into()]), &roles).unwrap();
    assert_eq!(
        bound.resolve(&roles).unwrap().column_acls["id"][0].role,
        "PUBLIC"
    );
    bound.column_acls.get_mut("id").unwrap()[0]
        .grant_options
        .select = true;
    assert!(bound.validate(Some(&["id".into()]), &roles).is_err());
}
