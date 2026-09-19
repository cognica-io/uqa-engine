//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn statement() -> uqa_sql::ast::RenameRoleStmt {
    uqa_sql::ast::RenameRoleStmt {
        name: "target".into(),
        new_name: "renamed".into(),
    }
}

#[test]
fn rename_publication_retains_identity_without_rewriting_memberships() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    let original = catalog.roles.borrow()["target"].clone();
    rename_role(&catalog.context(), &statement()).unwrap();
    let roles = catalog.roles.borrow();
    assert!(!roles.contains_key("target"));
    assert_eq!(roles["renamed"].identity(), original.identity());
    assert_eq!(roles["renamed"].revision, original.revision + 1);
    assert!(!catalog
        .events
        .borrow()
        .iter()
        .any(|event| event == "persist memberships"));
    assert_eq!(catalog.epoch.get(), 1);
}

#[test]
fn rename_rejects_an_intervening_tuple_change_before_reserving_the_destination() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    let mut committed = catalog.roles.borrow().clone();
    committed
        .get_mut("target")
        .unwrap()
        .advance_revision()
        .unwrap();
    catalog
        .refreshed_roles
        .borrow_mut()
        .push_back(committed.clone());
    assert_eq!(
        rename_role(&catalog.context(), &statement())
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert!(catalog.catalog_locks.borrow().is_empty());
    assert_eq!(*catalog.roles.borrow(), committed);
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn a_destination_committed_during_the_wait_is_a_uniqueness_error() {
    let catalog = Catalog::new();
    catalog.role("target", &[]);
    let original = catalog.roles.borrow().clone();
    catalog.role("renamed", &[]);
    let committed = catalog.roles.borrow().clone();
    *catalog.roles.borrow_mut() = original.clone();
    catalog
        .refreshed_roles
        .borrow_mut()
        .extend([original, committed.clone()]);
    assert_eq!(
        rename_role(&catalog.context(), &statement())
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    assert_eq!(*catalog.roles.borrow(), committed);
    assert_eq!(catalog.epoch.get(), 0);
    catalog.released();
}
