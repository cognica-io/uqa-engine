//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::roles::{RoleDefinition, RoleIdentity};

struct RefreshingDependencies<'a> {
    catalog: &'a Catalog,
    replacement: BTreeMap<String, RoleDefinition>,
    detail: Option<&'static str>,
}

impl super::super::context::RoleSharedDependencies for RefreshingDependencies<'_> {
    fn role_dependency_detail(&self, _: RoleIdentity) -> Result<Option<String>, SQLError> {
        // Catalog projection may restore committed role definitions while naming dependent objects.
        self.catalog.released();
        *self.catalog.roles.borrow_mut() = self.replacement.clone();
        Ok(self.detail.map(str::to_owned))
    }
}

#[test]
fn deletion_releases_roles_before_dependency_projection_and_keeps_diagnostic_order() {
    // PostgreSQL 18.4: GRANT EXECUTE ON FUNCTION role_function() TO dependent; DROP ROLE dependent.
    const DETAIL: &str = "privileges for function role_function()";
    for revision in [0, 1] {
        let catalog = Catalog::new();
        catalog.role("dependent", &[]);
        catalog
            .roles
            .borrow_mut()
            .get_mut("dependent")
            .unwrap()
            .revision = revision;
        let dependencies = RefreshingDependencies {
            catalog: &catalog,
            replacement: catalog.roles.borrow().clone(),
            detail: Some(DETAIL),
        };
        let context = RoleExecutionContext {
            dependencies: &dependencies,
            ..catalog.context()
        };
        let error = drop_roles(
            &context,
            &DropRoleStmt {
                names: vec!["dependent".into()],
                if_exists: false,
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, SQLError::Diagnostic { sqlstate, message, detail, hint }
            if sqlstate == "2BP01"
                && message == "role \"dependent\" cannot be dropped because some objects depend on it"
                && detail.as_deref() == Some(DETAIL)
                && hint.is_none())
        );
        assert!(catalog.roles.borrow().contains_key("dependent"));
        assert!(catalog.tuple_locks.borrow().is_empty());
        catalog.released();
    }
}

#[test]
fn deletion_preserves_the_selected_tuple_across_dependency_projection() {
    for change in ["unchanged", "updated", "replaced", "deleted"] {
        let catalog = Catalog::new();
        catalog.role("dependent", &[]);
        let mut replacement = catalog.roles.borrow().clone();
        match change {
            "updated" => replacement
                .get_mut("dependent")
                .unwrap()
                .advance_revision()
                .unwrap(),
            "replaced" => replacement.get_mut("dependent").unwrap().object_id = [99; 16],
            "deleted" => {
                replacement.remove("dependent");
            }
            _ => {}
        }
        let dependencies = RefreshingDependencies {
            catalog: &catalog,
            replacement,
            detail: None,
        };
        let context = RoleExecutionContext {
            dependencies: &dependencies,
            ..catalog.context()
        };
        let result = drop_roles(
            &context,
            &DropRoleStmt {
                names: vec!["dependent".into()],
                if_exists: false,
            },
        );
        if change == "unchanged" {
            result.unwrap();
            assert!(!catalog.roles.borrow().contains_key("dependent"));
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
            assert_eq!(*catalog.roles.borrow(), dependencies.replacement);
            assert_eq!(catalog.epoch.get(), 0);
        }
        catalog.released();
    }
}

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
