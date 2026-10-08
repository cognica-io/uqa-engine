//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{ast::RoleAttribute, catalog::roles::RoleDefinition};

fn binding(name: &str, types: &[&str]) -> FunctionBinding {
    FunctionBinding {
        name: name.into(),
        argument_types: types.iter().map(|ty| (*ty).into()).collect(),
        builtin: true,
        object_id: None,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

fn permissions() -> BuiltinRoutinePermissions {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    let reader = RoleDefinition {
        oid: 100,
        object_id: [7; 16],
        revision: 1,
        name: "reader".into(),
        attributes: std::collections::BTreeSet::from([RoleAttribute::Inherit]),
        connection_limit: -1,
    };
    snapshot.definitions.roles = Arc::new(BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader),
    ]));
    snapshot.definitions.builtin_routine_security = Arc::new(BTreeMap::from([
        (
            870,
            super::super::BuiltinRoutinePrivilegeUpdate::new(870, Vec::new())
                .unwrap()
                .entry,
        ),
        (
            2108,
            super::super::BuiltinRoutinePrivilegeUpdate::new(2108, Vec::new())
                .unwrap()
                .entry,
        ),
    ]));
    BuiltinRoutinePermissions::new(CatalogReadView::new(snapshot), "reader".into())
}

#[test]
fn retained_builtin_authority_uses_the_selected_declaration_and_kind() {
    let permissions = permissions();
    for (binding, message) in [
        (
            binding("pg_catalog.lower", &["text"]),
            "permission denied for function lower",
        ),
        (
            binding("sum", &["integer"]),
            "permission denied for aggregate sum",
        ),
    ] {
        let error = permissions.require_execute(&binding).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(error.to_string(), message);
    }
    // The other lower overload has a different catalog identity.
    permissions
        .require_execute(&binding("lower", &["integer"]))
        .unwrap();
    let mut user = binding("public.lower", &["text"]);
    user.builtin = false;
    user.object_id = Some([4; 16]);
    permissions.require_execute(&user).unwrap();
    permissions
        .require_execute(&binding("upper", &["text"]))
        .unwrap();
}

#[test]
fn builtin_permission_snapshots_and_bootstrap_authority_are_retained() {
    let revoked = permissions();
    let mut next = revoked.catalog.snapshot().clone();
    Arc::make_mut(&mut next.definitions.builtin_routine_security).clear();
    let allowed = BuiltinRoutinePermissions::new(CatalogReadView::new(next), "reader".into());
    let lower = binding("lower", &["text"]);
    allowed.require_execute(&lower).unwrap();
    assert!(revoked.require_execute(&lower).is_err());
    BuiltinRoutinePermissions::new(revoked.catalog.clone(), "uqa".into())
        .require_execute(&lower)
        .unwrap();
}
