//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    security::BoundTableSecurity, test_support::empty_catalog, CatalogTableSnapshot,
    RelationLookupMode,
};
use std::sync::Arc;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::ast::{
    ConstraintCatalogIdentity, Expr, PartitionBound, PartitionSpec, PartitionStrategy,
    RelationPersistence, TableHierarchy,
};
use uqa_storage::CatalogIndexRow;

fn table(hierarchy: TableHierarchy) -> CatalogTableSnapshot {
    CatalogTableSnapshot {
        object_id: [1; 16],
        security: Arc::new(BoundTableSecurity::owner(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        )),
        columns: Arc::default(),
        columns_declared: true,
        checks: Arc::default(),
        foreign_keys: Arc::default(),
        keys: Arc::default(),
        hierarchy: Arc::new(hierarchy),
        persistence: RelationPersistence::Permanent,
    }
}

fn resolution() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["unrelated".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Bound,
    }
}

fn index(name: &str, table: &str, unique: bool) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        index_type: "btree".into(),
        table_name: format!("public.{table}"),
        columns_json: r#"["value"]"#.into(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&super::super::IndexDefinition {
                unique,
                ..super::super::IndexDefinition::default()
            })
            .unwrap(),
        ),
    }
}

#[test]
fn selection_preserves_declared_identity_and_standalone_expression_predicate_and_null_semantics() {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "target"),
        table(TableHierarchy::default()),
    );
    let declared = TableKeyConstraint {
        catalog_identity: Some(ConstraintCatalogIdentity {
            object_id: [2; 16],
            oid: 50001,
        }),
        name: Some("declared".into()),
        kind: TableKeyConstraintKind::PrimaryKey,
        columns: vec!["value".into()],
        nulls_not_distinct: false,
        without_overlaps: false,
    };
    let mut unique = index("standalone", "target", true);
    let keys = vec![
        IndexKey::Column("value".into()),
        IndexKey::Expression(Box::new(Expr::Literal(Value::Int(1)))),
    ];
    unique.columns_json = serde_json::to_string(&keys).unwrap();
    let predicate = Some(Box::new(Expr::Literal(Value::Bool(true))));
    unique.definition_json = Some(
        serde_json::to_string(&super::super::IndexDefinition {
            unique: true,
            nulls_not_distinct: true,
            predicate: predicate.clone(),
            ..super::super::IndexDefinition::default()
        })
        .unwrap(),
    );
    let ordinary = index("ordinary", "target", false);
    snapshot.definitions.catalog_indexes = Arc::new(
        [
            (unique.relation.clone(), unique),
            (ordinary.relation.clone(), ordinary),
        ]
        .into(),
    );
    let keys = enforced_keys(
        &CatalogReadView::new(snapshot),
        &resolution(),
        "public.target",
        vec![declared.clone()],
    )
    .unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].constraint, declared);
    assert!(keys[0].constraint_owned);
    assert!(!keys[1].constraint_owned);
    assert_eq!(
        keys[1].index,
        Some(RelationIdentity::new("public", "standalone"))
    );
    assert_eq!(keys[1].keys.len(), 2);
    assert!(matches!(keys[1].keys[1], IndexKey::Expression(_)));
    assert_eq!(keys[1].predicate, predicate);
    assert!(keys[1].nulls_not_distinct);
    assert!(keys[1].catalog_identity.is_none());
    assert_eq!(keys[1].columns, ["value"]);
}

#[test]
fn partition_indexes_reach_descendants_but_ordinary_inheritance_and_unrelated_tables_do_not() {
    let mut snapshot = empty_catalog().snapshot().clone();
    for (name, parent, partitioned, partition) in [
        ("root", None, true, false),
        ("child", Some("root"), true, true),
        ("leaf", Some("child"), false, true),
        ("ordinary", None, false, false),
        ("inherited", Some("ordinary"), false, false),
    ] {
        snapshot.tables.insert(
            RelationIdentity::new("public", name),
            table(TableHierarchy {
                parents: parent
                    .into_iter()
                    .map(|parent| format!("public.{parent}"))
                    .collect(),
                partition_spec: partitioned.then(|| PartitionSpec {
                    strategy: PartitionStrategy::Range,
                    keys: vec![Expr::Column("value".into())],
                }),
                partition_bound: partition.then_some(PartitionBound::Default),
                ..TableHierarchy::default()
            }),
        );
    }
    snapshot.definitions.catalog_indexes = Arc::new(
        [
            index("root_key", "root", true),
            index("ordinary_key", "ordinary", true),
        ]
        .into_iter()
        .map(|index| (index.relation.clone(), index))
        .collect(),
    );
    let catalog = CatalogReadView::new(snapshot);
    for target in ["public.root", "public.child", "public.leaf"] {
        let keys = enforced_keys(&catalog, &resolution(), target, Vec::new()).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name.as_deref(), Some("root_key"));
    }
    assert!(
        enforced_keys(&catalog, &resolution(), "public.inherited", Vec::new())
            .unwrap()
            .is_empty()
    );
    let keys = enforced_keys(&catalog, &resolution(), "public.ordinary", Vec::new()).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name.as_deref(), Some("ordinary_key"));
}
