//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::SchemaSecurity;
use crate::{
    ast::RelationPersistence,
    catalog::roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition, RoleMembership, RoleMembershipKey,
    },
    plan::UnifiedPlan,
};
use std::{cell::RefCell, collections::BTreeMap};

struct Catalog {
    user: String,
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    calls: RefCell<Vec<&'static str>>,
}
impl Catalog {
    fn new(user: &str) -> Self {
        Self {
            user: user.into(),
            roles: RefCell::new(BTreeMap::new()),
            memberships: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn context(&self) -> ViewOwnershipContext<'_> {
        ViewOwnershipContext {
            session: self,
            roles: self,
            schemas: self,
        }
    }
}
impl RoleReferenceNames for Catalog {
    fn current_user_name(&self) -> String {
        self.calls.borrow_mut().push("current_user");
        self.user.clone()
    }
    fn session_user_name(&self) -> String {
        panic!("view ownership uses the current role");
    }
}
impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        assert!(self.memberships.try_borrow_mut().is_ok());
        self.calls.borrow_mut().push("roles");
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        assert!(self.roles.try_borrow_mut().is_err());
        self.calls.borrow_mut().push("memberships");
        Box::new(self.memberships.borrow())
    }
}
impl ViewOwnerSchemas for Catalog {
    fn schema_security(&self, _: &str) -> Option<SchemaSecurity> {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
        self.calls.borrow_mut().push("schema");
        Some(SchemaSecurity {
            role_owner: "schema_owner".into(),
            acl: None,
        })
    }
}
fn view(kind: StoredViewKind) -> StoredView {
    let statement = crate::compile("SELECT 1 AS value").unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query fixture");
    };
    StoredView {
        object_id: [1; 16],
        role_owner: "view_owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["value".into()]),
        persistence: RelationPersistence::Permanent,
        options: Vec::new(),
        kind,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}

#[test]
fn view_owner_success_precedes_name_parsing_and_retains_role_guard_order() {
    let catalog = Catalog::new("view_owner");
    let view = view(StoredViewKind::View);
    assert_eq!(
        ensure_view_owner(catalog.context(), "\"unterminated", &view).unwrap(),
        "view_owner"
    );
    assert_eq!(
        *catalog.calls.borrow(),
        ["current_user", "roles", "memberships"]
    );
    assert!(catalog.roles.try_borrow_mut().is_ok());
    assert!(catalog.memberships.try_borrow_mut().is_ok());
}

#[test]
fn schema_owner_drop_authority_does_not_grant_view_replacement_or_maintenance() {
    let catalog = Catalog::new("schema_owner");
    let regular = view(StoredViewKind::View);
    ensure_view_drop_authority(catalog.context(), "public.v", &regular).unwrap();
    assert_eq!(
        *catalog.calls.borrow(),
        [
            "current_user",
            "roles",
            "memberships",
            "schema",
            "current_user",
            "roles",
            "memberships"
        ]
    );
    let error = ensure_view_owner(catalog.context(), "public.v", &regular).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error.to_string().contains("must be owner of view v"));
    let error = ensure_materialized_view_maintenance(
        catalog.context(),
        "public.mv",
        &view(StoredViewKind::Materialized),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error
        .to_string()
        .contains("permission denied for materialized view mv"));
}

#[test]
fn view_drop_parses_the_target_before_reading_role_or_schema_catalogs() {
    let catalog = Catalog::new("view_owner");
    let error = ensure_view_drop_authority(
        catalog.context(),
        "\"unterminated",
        &view(StoredViewKind::View),
    )
    .unwrap_err();
    assert!(error.to_string().contains("resolve view"));
    assert!(catalog.calls.borrow().is_empty());
}
