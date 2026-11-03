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

#[test]
fn captured_table_security_keeps_full_acl_tuple_identity_and_releases_failed_copies() {
    let roles = roles();
    let mut source = security(&roles);
    source.acl_revisions.relation = Some([7; 16]);
    source.acl_revisions.columns.insert("id".into(), [8; 16]);
    // A removed column ACL can still have a revision recording its deletion.
    source
        .acl_revisions
        .columns
        .insert("retired".into(), [9; 16]);
    let memory = uqa_core::memory::MemoryBudget::new(1 << 20);
    let cancellation = uqa_core::CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&memory, &cancellation, &cancellation);
    let captured = source
        .capture_security(&control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    assert_eq!(captured.privileges.role_owner, source.role_owner);
    assert_eq!(captured.privileges.acl(), source.acl.as_deref());
    assert_eq!(
        captured.privileges.column_acls(),
        &source
            .column_acls
            .iter()
            .map(|(name, acl)| (name.clone(), acl.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(captured.relation_revision, Some([7; 16]));
    assert_eq!(
        captured.column_revisions,
        [("id".into(), [8; 16]), ("retired".into(), [9; 16])]
    );
    drop(source);
    assert_eq!(
        captured.privileges.resolve(&roles).unwrap().role_owner,
        "owner"
    );
    drop(captured);
    assert_eq!(memory.used(), 0);
    let source = security(&roles);
    for limit in [0, 32, 128] {
        let memory = uqa_core::memory::MemoryBudget::new(limit);
        let control =
            uqa_core::memory::ProductionControl::new(&memory, &cancellation, &cancellation);
        assert!(source.capture_security(&control).is_err());
        assert_eq!(memory.used(), 0);
    }
    cancellation.cancel();
    assert!(matches!(
        source.capture_security(&control),
        Err(crate::SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

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
        AclGrantee::Public
    );
    bound.column_acls.get_mut("id").unwrap()[0]
        .grant_options
        .select = true;
    assert!(bound.validate(Some(&["id".into()]), &roles).is_err());
}

#[test]
fn named_public_and_public_paths_bind_and_resolve_independently() {
    let mut roles = roles();
    let mut named_public = roles["reader"].clone();
    named_public.name = "PUBLIC".into();
    named_public.oid += 100;
    named_public.object_id = [9; 16];
    roles.insert(named_public.name.clone(), named_public.clone());
    let mut named = TableSecurity::owner("owner");
    grant_acl(
        &mut named,
        TableAclPrivilege::Select,
        &["PUBLIC".into()],
        "owner",
        true,
    );
    grant_acl(
        &mut named,
        TableAclPrivilege::Select,
        &[AclGrantee::Public],
        "owner",
        false,
    );
    grant_column_acl(
        &mut named,
        "id",
        TableAclPrivilege::Select,
        &["reader".into()],
        "PUBLIC",
        false,
    );
    let bound = BoundTableSecurity::bind(&named, &roles).unwrap();
    bound.validate(Some(&["id".into()]), &roles).unwrap();
    assert_eq!(bound.resolve(&roles).unwrap(), named);
    assert!(bound.depends_on(named_public.identity()));

    roles.remove("PUBLIC");
    named_public.name = "renamed_public".into();
    roles.insert(named_public.name.clone(), named_public);
    let restored = bound.resolve(&roles).unwrap();
    let acl = restored.acl.as_ref().unwrap();
    assert!(acl.iter().any(|entry| entry.role == AclGrantee::Public));
    assert!(acl.iter().any(
        |entry| entry.role.role_name() == Some("renamed_public") && entry.grant_options.select
    ));
    assert_eq!(
        restored.column_acls["id"][0].grantor.as_deref(),
        Some("renamed_public")
    );
    bound.validate(Some(&["id".into()]), &roles).unwrap();
    assert_eq!(BoundTableSecurity::bind(&restored, &roles).unwrap(), bound);
}

mod production;
