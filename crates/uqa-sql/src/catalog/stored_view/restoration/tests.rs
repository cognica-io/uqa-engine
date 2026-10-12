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
                    object_id: [42; 16],
                    revision: 1,
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
        security: crate::catalog::security::BoundTableSecurity::owner(
            crate::catalog::roles::RoleIdentity {
                oid: 42,
                object_id: [42; 16],
            },
        ),
        definition: crate::catalog::stored_view::StoredViewDefinition {
            object_id: [7; 16],
            catalog_oids: None,
            row_type_array_name: None,
            query: *query,
            output_columns: Some(vec!["value".into()]),
            persistence: RelationPersistence::Permanent,
            options: Vec::new(),
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            materialized_column_collations: None,
            populated: true,
        },
    }
}

#[test]
fn materialized_collations_survive_codecs_and_reject_missing_or_malformed_metadata() {
    let mut original = view();
    original.kind = StoredViewKind::Materialized;
    original.output_columns = Some(vec!["c".into(), "p".into(), "n".into()]);
    original.materialized_column_types = vec![
        Some(crate::ColumnType::Text),
        Some(crate::ColumnType::Text),
        Some(crate::ColumnType::Integer),
    ];
    original.materialized_column_collations = Some(vec![950, 951, 0]);
    let encoded = serde_json::to_value(&original).unwrap();
    let restored: super::super::StoredViewDefinition =
        serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(
        restored.materialized_schema().unwrap().column_collation(0),
        Some(950)
    );
    assert_eq!(
        restored.materialized_schema().unwrap().column_collation(1),
        Some(951)
    );
    for invalid in [
        serde_json::Value::Null,
        serde_json::json!([950]),
        serde_json::json!([0, 951, 0]),
        serde_json::json!([950, 951, 100]),
    ] {
        let mut data = encoded.clone();
        data["materialized_column_collations"] = invalid;
        let restored: super::super::StoredViewDefinition = serde_json::from_value(data).unwrap();
        assert!(restored.materialized_schema().is_err());
    }
    let mut legacy = encoded;
    legacy
        .as_object_mut()
        .unwrap()
        .remove("materialized_column_collations");
    let mut restored: super::super::StoredViewDefinition = serde_json::from_value(legacy).unwrap();
    let schema = crate::RowSchema::with_types(
        vec!["x".into(), "y".into(), "z".into()],
        original.materialized_column_types.clone(),
    )
    .with_column_collations([Some(950), Some(951), Some(0)]);
    restored.restore_materialized_collations(&schema).unwrap();
    assert_eq!(
        restored.materialized_column_collations,
        original.materialized_column_collations
    );
    assert_eq!(restored.output_columns, original.output_columns);
    let mut malformed = restored.clone();
    malformed.materialized_column_collations = None;
    let wrong =
        crate::RowSchema::with_types(vec!["x".into()], vec![Some(crate::ColumnType::Integer)]);
    assert!(malformed.restore_materialized_collations(&wrong).is_err());
    assert!(malformed.materialized_column_collations.is_none());
}

#[test]
fn materialized_metadata_preserves_native_deferred_columns_and_existing_selections() {
    let mut stored = view();
    stored.kind = StoredViewKind::Materialized;
    stored.materialized_column_types = vec![None];
    let schema = crate::RowSchema::with_types(vec!["value".into()], vec![None]);
    stored.restore_materialized_collations(&schema).unwrap();
    assert_eq!(stored.materialized_column_collations, Some(vec![0]));
    assert_eq!(stored.materialized_schema().unwrap().column_type(0), None);
    let changed =
        crate::RowSchema::with_types(vec!["value".into()], vec![Some(crate::ColumnType::Text)])
            .with_column_collations([Some(951)]);
    stored.restore_materialized_collations(&changed).unwrap();
    assert_eq!(stored.materialized_column_types, [None]);
    assert_eq!(stored.materialized_column_collations, Some(vec![0]));
}

#[test]
fn metadata_migration_orders_view_dependencies_without_copying_queries() {
    let definitions = [
        ("a_dependent", "SELECT value FROM public.middle"),
        ("middle", "SELECT value FROM public.z_base"),
        ("z_base", "SELECT 1 AS value"),
        ("unrelated", "SELECT value FROM public.unrelated"),
    ]
    .into_iter()
    .map(|(name, sql)| {
        let mut view = view();
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        view.query = *query;
        (RelationIdentity::new("public", name), view)
    })
    .collect::<BTreeMap<_, _>>();
    let targets = ["a_dependent", "z_base"]
        .map(|name| RelationIdentity::new("public", name))
        .into();
    let ordered = view_metadata_dependency_order(&definitions, &targets).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|relation| relation.name.as_str())
            .collect::<Vec<_>>(),
        ["z_base", "a_dependent"]
    );
    let cyclic = [RelationIdentity::new("public", "unrelated")].into();
    assert!(view_metadata_dependency_order(&definitions, &cyclic)
        .unwrap_err()
        .to_string()
        .contains("recursive dependency"));
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
    assert!(serde_json::to_value(&current)
        .unwrap()
        .get("role_owner")
        .is_none());
    let RestoredView::Legacy(legacy) =
        serde_json::from_str(&serde_json::to_string(&original.query).unwrap()).unwrap()
    else {
        panic!("query-only format");
    };
    assert_eq!(
        serde_json::to_value(legacy).unwrap(),
        serde_json::to_value(original.definition.query).unwrap()
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
    assert_eq!(roles.reads.get(), 1);
    roles.reads.set(0);
    candidate.security.role_owner.object_id = [99; 16];
    let error = validate_restored_view_security(&roles, "public.v", &candidate).unwrap_err();
    assert!(error.contains("missing role incarnation"));
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
