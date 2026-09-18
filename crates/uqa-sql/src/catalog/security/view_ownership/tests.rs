//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::RoleReference;
use crate::catalog::security::BoundSchemaSecurity;
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
        let roles = ["view_owner", "schema_owner", user]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
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
            .collect();
        Self {
            user: user.into(),
            roles: RefCell::new(roles),
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
    fn current_role(&self) -> RoleReference {
        self.calls.borrow_mut().push("current_user");
        self.user.clone().into()
    }
    fn session_role(&self) -> RoleReference {
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
    fn schema_security(&self, _: &str) -> Option<BoundSchemaSecurity> {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
        self.calls.borrow_mut().push("schema");
        Some(BoundSchemaSecurity {
            role_owner: self.roles.borrow()["schema_owner"].identity(),
            acl: None,
        })
    }
}
fn view(catalog: &Catalog, kind: StoredViewKind) -> StoredView {
    let statement = crate::compile("SELECT 1 AS value").unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query fixture");
    };
    StoredView {
        security: crate::catalog::security::BoundTableSecurity::owner(
            catalog.roles.borrow()["view_owner"].identity(),
        ),
        definition: crate::catalog::stored_view::StoredViewDefinition {
            object_id: [1; 16],
            query: *query,
            output_columns: Some(vec!["value".into()]),
            persistence: RelationPersistence::Permanent,
            options: Vec::new(),
            kind,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
        },
    }
}

#[test]
fn view_owner_success_precedes_name_parsing_and_retains_role_guard_order() {
    let catalog = Catalog::new("view_owner");
    let view = view(&catalog, StoredViewKind::View);
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
    let regular = view(&catalog, StoredViewKind::View);
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
        &view(&catalog, StoredViewKind::Materialized),
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
        &view(&catalog, StoredViewKind::View),
    )
    .unwrap_err();
    assert!(error.to_string().contains("resolve view"));
    assert!(catalog.calls.borrow().is_empty());
}
