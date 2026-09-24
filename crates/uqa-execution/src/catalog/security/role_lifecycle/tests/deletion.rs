//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role deletion retains target order across waits and membership changes.

use super::*;
use uqa_sql::ast::RoleSpecification;

#[test]
fn deletion_checks_a_later_target_after_the_first_object_wait() {
    for removed in [false, true] {
        let catalog = Catalog::new();
        catalog.role("first", &[]);
        if removed {
            let mut refreshed = catalog.roles.borrow().clone();
            refreshed.remove("first");
            catalog.refreshed_roles.borrow_mut().push_back(refreshed);
        }
        let statement = DropRoleStmt {
            names: vec!["first".into(), RoleSpecification::CurrentUser],
            if_exists: false,
        };
        assert_eq!(
            drop_roles(&catalog.context(), &statement)
                .unwrap_err()
                .sqlstate(),
            Some("22023")
        );
        assert_eq!(catalog.catalog_locks.borrow().len(), 1);
        assert!(catalog.tuple_locks.borrow().is_empty());
        assert!(!catalog
            .events
            .borrow()
            .iter()
            .any(|event| event == "persist roles"));
        catalog.released();
    }
}

#[test]
fn deletion_checks_later_administration_after_removing_prior_memberships() {
    for reverse in [false, true] {
        let catalog = Catalog::new();
        catalog.role("actor", &[RoleAttribute::CreateRole]);
        catalog.role("first", &[RoleAttribute::Inherit]);
        catalog.role("second", &[]);
        catalog.membership("first", "actor", "uqa");
        catalog.membership("second", "first", "uqa");
        for membership in catalog.memberships.borrow_mut().values_mut() {
            membership.inherit_option = true;
        }
        *catalog.current.borrow_mut() = "actor".into();
        let names = if reverse {
            ["second", "first"]
        } else {
            ["first", "second"]
        };
        let result = drop_roles(
            &catalog.context(),
            &DropRoleStmt {
                names: names.map(RoleSpecification::from).to_vec(),
                if_exists: false,
            },
        );
        if reverse {
            result.unwrap();
            assert!(!catalog.roles.borrow().contains_key("first"));
            assert!(!catalog.roles.borrow().contains_key("second"));
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
            assert!(catalog.roles.borrow().contains_key("first"));
            assert!(catalog.roles.borrow().contains_key("second"));
        }
        catalog.released();
    }
}
