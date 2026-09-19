//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn alteration() -> AlterRoleStmt {
    AlterRoleStmt {
        name: "target".into(),
        attributes: BTreeMap::from([(RoleAttribute::Login, true)]),
        connection_limit: None,
        membership_action: None,
        members: Vec::new(),
    }
}

#[test]
fn alteration_does_not_retry_a_committed_definition_even_when_attributes_match() {
    for change_attribute in [false, true] {
        let catalog = Catalog::new();
        catalog.role("target", &[]);
        let mut committed = catalog.roles.borrow().clone();
        let target = committed.get_mut("target").unwrap();
        target.advance_revision().unwrap();
        if change_attribute {
            target.attributes.insert(RoleAttribute::Login);
        }
        catalog
            .refreshed_roles
            .borrow_mut()
            .push_back(committed.clone());
        assert_eq!(
            alter_role(&catalog.context(), &alteration())
                .unwrap_err()
                .sqlstate(),
            Some("XX000")
        );
        assert_eq!(*catalog.roles.borrow(), committed);
        assert_eq!(catalog.epoch.get(), 0);
        assert!(!catalog
            .events
            .borrow()
            .iter()
            .any(|event| event == "persist roles"));
        catalog.released();
    }
}

#[test]
fn tuple_publication_preserves_an_unrelated_definition_committed_during_the_wait() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    catalog.role("peer", &[]);
    let mut committed = catalog.roles.borrow().clone();
    let peer = committed.get_mut("peer").unwrap();
    peer.attributes.insert(RoleAttribute::Login);
    peer.advance_revision().unwrap();
    catalog
        .refreshed_roles
        .borrow_mut()
        .push_back(committed.clone());
    alter_role(&catalog.context(), &alteration()).unwrap();
    let roles = catalog.roles.borrow();
    assert_eq!(roles["peer"], committed["peer"]);
    assert!(roles["target"].has(RoleAttribute::Login));
    assert_eq!(roles["target"].revision, 2);
}

#[test]
fn denied_alteration_never_waits_for_a_definition_tuple() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    catalog.role("actor", &[RoleAttribute::CreateRole]);
    *catalog.current.borrow_mut() = "actor".into();
    assert_eq!(
        alter_role(&catalog.context(), &alteration())
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert!(catalog.tuple_locks.borrow().is_empty());
    assert!(catalog.catalog_locks.borrow().is_empty());
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn deletion_revalidates_the_tuple_selected_after_dependency_exclusion() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    let original = catalog.roles.borrow().clone();
    let mut committed = original.clone();
    committed
        .get_mut("target")
        .unwrap()
        .advance_revision()
        .unwrap();
    catalog
        .refreshed_roles
        .borrow_mut()
        .extend([original, committed.clone()]);
    let statement = DropRoleStmt {
        names: vec!["target".into()],
        if_exists: false,
    };
    assert_eq!(
        drop_roles(&catalog.context(), &statement)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(*catalog.roles.borrow(), committed);
    assert_eq!(catalog.epoch.get(), 0);
    assert!(!catalog
        .events
        .borrow()
        .iter()
        .any(|event| event == "persist roles"));
    catalog.released();
}
