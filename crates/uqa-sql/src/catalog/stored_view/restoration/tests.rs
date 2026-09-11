//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    catalog::{
        roles::{
            guards::{RoleDefinitionRead, RoleMembershipRead},
            RoleDefinition,
        },
        view::StoredViewKind,
    },
    plan::UnifiedPlan,
};
use std::cell::{Cell, RefCell};

struct Roles {
    definitions: RefCell<BTreeMap<String, RoleDefinition>>,
    reads: Cell<usize>,
}
impl Roles {
    fn new() -> Self {
        Self {
            definitions: RefCell::new(BTreeMap::from([(
                "owner".into(),
                RoleDefinition {
                    oid: 42,
                    name: "owner".into(),
                    attributes: BTreeSet::new(),
                    connection_limit: -1,
                },
            )])),
            reads: Cell::new(0),
        }
    }
}
impl RoleCatalogGuards for Roles {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        assert!(
            self.definitions.try_borrow_mut().is_ok(),
            "the previous role guard must be released before a fresh lookup"
        );
        self.reads.set(self.reads.get() + 1);
        Box::new(self.definitions.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        panic!("persisted ACL validation reads role definitions only");
    }
}
fn view() -> StoredView {
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(crate::compile("SELECT 1 AS value").unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    StoredView {
        object_id: [7; 16],
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["value".into()]),
        persistence: RelationPersistence::Permanent,
        options: Vec::new(),
        kind: StoredViewKind::View,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}

#[test]
fn restored_view_decoder_preserves_current_and_query_only_formats() {
    let original = view();
    let RestoredView::Current(current) =
        serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap()
    else {
        panic!("current format");
    };
    assert_eq!(current.object_id, original.object_id);
    assert_eq!(current.output_columns, original.output_columns);
    assert!(current.role_owner.is_empty());
    let RestoredView::Legacy(legacy) =
        serde_json::from_str(&serde_json::to_string(&original.query).unwrap()).unwrap()
    else {
        panic!("query-only format");
    };
    assert_eq!(
        serde_json::to_value(legacy).unwrap(),
        serde_json::to_value(original.query).unwrap()
    );
}

#[test]
fn restored_view_identity_validation_includes_temporary_definitions() {
    let mut temporary = view();
    temporary.persistence = RelationPersistence::Temporary;
    let mut views = BTreeMap::from([
        (RelationIdentity::new("public", "v"), view()),
        (RelationIdentity::new("pg_temp_7", "v"), temporary),
    ]);
    let error = validate_restored_view_object_ids(&views).unwrap_err();
    assert!(error.contains("duplicate object identity"));
    views
        .get_mut(&RelationIdentity::new("pg_temp_7", "v"))
        .unwrap()
        .object_id = [8; 16];
    validate_restored_view_object_ids(&views).unwrap();
}

#[test]
fn restored_view_security_keeps_owner_and_public_metadata_error_precedence() {
    let roles = Roles::new();
    let mut candidate = view();
    validate_restored_view_security(&roles, "public.v", &candidate).unwrap();
    assert_eq!(roles.reads.get(), 2);
    roles.reads.set(0);
    candidate.role_owner = "missing".into();
    let error = validate_restored_view_security(&roles, "public.v", &candidate).unwrap_err();
    assert!(error.contains("owned by missing role `missing`"));
    assert_eq!(roles.reads.get(), 1);
    roles.reads.set(0);
    candidate.output_columns = None;
    let error = validate_migrated_view_security(
        &roles,
        &BTreeMap::from([(RelationIdentity::new("public", "v"), candidate)]),
    )
    .unwrap_err();
    assert!(error.contains("no public column metadata after catalog migration"));
    assert_eq!(roles.reads.get(), 0);
}
