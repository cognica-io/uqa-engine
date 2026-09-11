//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{RelationPersistence, Statement},
    catalog::{
        roles::{
            guards::{RoleDefinitionRead, RoleMembershipRead},
            RoleDefinition, RoleMembership, RoleMembershipKey,
        },
        security::columns::grant_column_acl,
    },
    plan::UnifiedPlan,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};

#[derive(Default)]
struct Catalog {
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    calls: RefCell<Vec<&'static str>>,
    reads: Cell<usize>,
    promote_on_recheck: bool,
}
impl Catalog {
    fn context(&self) -> ViewAuthorizationContext<'_> {
        ViewAuthorizationContext { roles: self }
    }
    fn assert_released(&self) {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
    }
}
impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        self.assert_released();
        if self.promote_on_recheck && self.reads.get() == 1 {
            let Statement::CreateRole(role) = crate::compile("CREATE ROLE reader SUPERUSER")
                .unwrap()
                .remove(0)
            else {
                panic!("expected role")
            };
            self.roles
                .borrow_mut()
                .insert("reader".into(), RoleDefinition::from_create(&role));
        }
        self.reads.set(self.reads.get() + 1);
        self.calls.borrow_mut().push("roles");
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        assert!(self.roles.try_borrow_mut().is_err());
        assert!(self.memberships.try_borrow_mut().is_ok());
        self.calls.borrow_mut().push("memberships");
        Box::new(self.memberships.borrow())
    }
}
fn view(kind: StoredViewKind) -> StoredView {
    let statement = crate::compile("SELECT 1 AS allowed, 2 AS hidden")
        .unwrap()
        .remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("expected query")
    };
    StoredView {
        object_id: [1; 16],
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["allowed".into(), "hidden".into()]),
        persistence: RelationPersistence::Permanent,
        options: vec![],
        kind,
        materialized_rows: vec![],
        materialized_column_types: vec![],
        populated: true,
    }
}
fn grant_column(view: &mut StoredView) {
    let mut security = view.security();
    grant_column_acl(
        &mut security,
        "allowed",
        TableAclPrivilege::Select,
        &["reader".into()],
        "owner",
        false,
    );
    view.column_acls = security.column_acls;
}

#[test]
fn ordinary_view_checks_parse_names_before_reading_authorization_catalogs() {
    let catalog = Catalog::default();
    let view = view(StoredViewKind::View);
    assert!(catalog
        .context()
        .ensure_view_privilege_for("\"unterminated", &view, "owner", TableAclPrivilege::Select)
        .is_err());
    assert!(catalog
        .context()
        .ensure_view_column_privilege_for(
            "\"unterminated",
            &view,
            "allowed",
            "owner",
            TableAclPrivilege::Select
        )
        .is_err());
    assert!(catalog.calls.borrow().is_empty());
}

#[test]
fn any_column_requires_public_metadata_before_owner_success_or_name_parsing() {
    let catalog = Catalog::default();
    let mut view = view(StoredViewKind::View);
    view.output_columns = None;
    let error = catalog
        .context()
        .ensure_any_view_column_privilege_for(
            "\"unterminated",
            &view,
            "owner",
            TableAclPrivilege::Select,
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("has no durable public column metadata"));
    assert_eq!(*catalog.calls.borrow(), ["roles", "memberships"]);
    catalog.assert_released();
}

#[test]
fn granted_public_column_satisfies_any_column_without_reparsing_the_bound_name() {
    let catalog = Catalog::default();
    let mut view = view(StoredViewKind::View);
    grant_column(&mut view);
    catalog
        .context()
        .ensure_any_view_column_privilege_for(
            "\"unterminated",
            &view,
            "reader",
            TableAclPrivilege::Select,
        )
        .unwrap();
    assert_eq!(*catalog.calls.borrow(), ["roles", "memberships"]);
    catalog.assert_released();
}

#[test]
fn column_access_does_not_authorize_another_column_or_table_wide_access() {
    let catalog = Catalog::default();
    let mut view = view(StoredViewKind::View);
    grant_column(&mut view);
    catalog
        .context()
        .ensure_view_column_privilege_for(
            "public.visible",
            &view,
            "allowed",
            "reader",
            TableAclPrivilege::Select,
        )
        .unwrap();
    for error in [
        catalog
            .context()
            .ensure_view_column_privilege_for(
                "public.visible",
                &view,
                "hidden",
                "reader",
                TableAclPrivilege::Select,
            )
            .unwrap_err(),
        catalog
            .context()
            .ensure_view_privilege_for("public.visible", &view, "reader", TableAclPrivilege::Select)
            .unwrap_err(),
    ] {
        assert_eq!(error.sqlstate(), Some("42501"));
        assert!(error
            .to_string()
            .contains("permission denied for view visible"));
    }
    catalog.assert_released();
}

#[test]
fn denied_any_column_releases_both_guards_before_a_fresh_authorization_recheck() {
    let catalog = Catalog {
        promote_on_recheck: true,
        ..Catalog::default()
    };
    catalog
        .context()
        .ensure_any_view_column_privilege_for(
            "public.visible",
            &view(StoredViewKind::View),
            "reader",
            TableAclPrivilege::Select,
        )
        .unwrap();
    assert_eq!(
        *catalog.calls.borrow(),
        ["roles", "memberships", "roles", "memberships"]
    );
    catalog.assert_released();
}

#[test]
fn denied_materialized_view_keeps_its_relation_kind_in_the_fallback_error() {
    let catalog = Catalog::default();
    let error = catalog
        .context()
        .ensure_any_view_column_privilege_for(
            "public.saved",
            &view(StoredViewKind::Materialized),
            "reader",
            TableAclPrivilege::Select,
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error
        .to_string()
        .contains("permission denied for materialized view saved"));
    assert_eq!(
        *catalog.calls.borrow(),
        ["roles", "memberships", "roles", "memberships"]
    );
    catalog.assert_released();
}
