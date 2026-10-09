//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{CatalogReadSnapshot, CatalogTableSnapshot};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{catalog_role::RoleIdentity, Value};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{
        composite_type::StoredComposite,
        domain::StoredDomain,
        enum_type::{initial_enum_labels, StoredEnum},
        relation_oids::{RelationCatalogOids, RelationOidKind},
        security::BoundTableSecurity,
    },
};

fn table(oid: u32) -> CatalogTableSnapshot {
    CatalogTableSnapshot {
        dropped_attributes: Arc::default(),
        object_id: [1; 16],
        catalog_oids: RelationCatalogOids {
            relation: oid,
            row_type: Some(oid + 1),
            array_type: Some(oid + 2),
            rule: None,
        },
        row_type_array_name: None,
        security: Arc::new(BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP)),
        columns: Arc::default(),
        columns_declared: true,
        checks: Arc::default(),
        foreign_keys: Arc::default(),
        keys: Arc::default(),
        hierarchy: Arc::default(),
        persistence: RelationPersistence::Permanent,
    }
}

fn add_user_types(snapshot: &mut CatalogReadSnapshot) {
    let uqa_sql::Statement::CreateDomain(definition) =
        uqa_sql::compile("CREATE DOMAIN public.positive AS integer CHECK (VALUE > 0)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let domain = StoredDomain {
        object_id: [2; 16],
        oid: 60_000,
        array_oid: Some(60_001),
        identity: RelationIdentity::new("public", "positive"),
        owner: RoleIdentity::BOOTSTRAP,
        definition,
        array_name: None,
        usage_acl: None,
    };
    Arc::make_mut(&mut snapshot.definitions.domains).insert("public.positive".into(), domain);
    let mut legacy = snapshot.definitions.domains["public.positive"].clone();
    legacy.oid = 60_002;
    legacy.array_oid = None;
    legacy.identity.name = "legacy".into();
    Arc::make_mut(&mut snapshot.definitions.domains).insert("public.legacy".into(), legacy);
    let definition = StoredEnum {
        object_id: [3; 16],
        oid: 60_010,
        array_oid: 60_011,
        array_name: "_mood".into(),
        identity: RelationIdentity::new("public", "mood"),
        owner: RoleIdentity::BOOTSTRAP,
        labels: initial_enum_labels(60_010, &["calm".into()], &[60_012]).unwrap(),
        usage_acl: None,
    };
    snapshot.definitions.enums = Arc::new(BTreeMap::from([("public.mood".into(), definition)]));
    snapshot.definitions.composites = Arc::new(BTreeMap::from([(
        "public.pair".into(),
        StoredComposite {
            object_id: [4; 16],
            oid: 60_020,
            array_oid: 60_021,
            relation_oid: 60_022,
            array_name: "_pair".into(),
            identity: RelationIdentity::new("public", "pair"),
            owner: RoleIdentity::BOOTSTRAP,
            attributes: Vec::new(),
            usage_acl: None,
        },
    )]));
}

fn add_relations(snapshot: &mut CatalogReadSnapshot, tables: u32) {
    for index in 0..tables {
        let mut definition = table(70_000 + 3 * index);
        if index == 0 {
            definition.persistence = RelationPersistence::Temporary;
        }
        snapshot.tables.insert(
            RelationIdentity::new("public", format!("t{index}")),
            definition,
        );
    }
    let uqa_sql::plan::UnifiedPlan::Query(query) =
        uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS value").unwrap().remove(0))
    else {
        unreachable!()
    };
    for (name, kind, object) in [
        ("v", uqa_sql::catalog::view::StoredViewKind::View, 5),
        ("m", uqa_sql::catalog::view::StoredViewKind::Materialized, 6),
    ] {
        let view = crate::catalog::view::StoredView {
            security: BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
            definition: uqa_sql::catalog::stored_view::StoredViewDefinition {
                object_id: [object; 16],
                catalog_oids: None,
                row_type_array_name: None,
                query: *query.clone(),
                output_columns: Some(vec!["value".into()]),
                persistence: RelationPersistence::Permanent,
                options: Vec::new(),
                kind,
                materialized_rows: Vec::new(),
                materialized_column_types: Vec::new(),
                populated: true,
            },
        };
        Arc::make_mut(&mut snapshot.definitions.views)
            .insert(RelationIdentity::new("public", name), view);
    }
    let foreign = crate::catalog::foreign::StoredForeignTable {
        dropped_attributes: Vec::new(),
        name: "public.external".into(),
        persistence: RelationPersistence::Permanent,
        object_id: [7; 16],
        catalog_oids: None,
        row_type_array_name: None,
        server_name: "memory".into(),
        server_reference: None,
        columns: Vec::new(),
        checks: Vec::new(),
        options: BTreeMap::default(),
        option_order: Vec::new(),
    };
    snapshot.definitions.foreign_tables = Arc::new(BTreeMap::from([(
        RelationIdentity::new("public", "external"),
        foreign,
    )]));
}

fn projected_oids(catalog: &CatalogReadView) -> BTreeSet<i64> {
    crate::catalog::projection::pg_catalog::build_pg_type_without_defaults(catalog, &resolution())
        .unwrap()
        .iter()
        .map(|row| match row["oid"] {
            Value::Int(oid) => oid,
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn type_membership_matches_complete_projection_without_producing_rows() {
    for table_count in [0, 1, 256] {
        let mut snapshot = empty_catalog().snapshot().clone();
        add_user_types(&mut snapshot);
        add_relations(&mut snapshot, table_count);
        let catalog = CatalogReadView::new(snapshot);
        let expected = projected_oids(&catalog);
        let before = TYPE_PROJECTION_BUILDS.get();
        for oid in expected.iter().copied().chain([
            -1,
            0,
            60_003,
            60_012,
            60_022,
            i64::from(u32::MAX),
            i64::MAX,
        ]) {
            assert_eq!(
                catalog_oid_in_use(&catalog, &resolution(), CatalogOidClass::Type, oid).unwrap(),
                expected.contains(&oid),
                "OID {oid}"
            );
        }
        assert_eq!(TYPE_PROJECTION_BUILDS.get(), before);
        for oid in [
            23, 1007, 705, 2249, 2287, 2278, 60_000, 60_001, 60_002, 60_010, 60_011, 60_020, 60_021,
        ] {
            assert!(expected.contains(&oid), "missing fixture type {oid}");
        }
        assert!(
            expected.contains(&uqa_sql::catalog::type_metadata::pg_domain_array_oid(
                60_002, None
            ))
        );
        for (kind, object) in [
            (RelationOidKind::View, 5),
            (RelationOidKind::View, 6),
            (RelationOidKind::ForeignTable, 7),
        ] {
            let oids = RelationCatalogOids::legacy(kind, &[object; 16]);
            assert!(expected.contains(&i64::from(oids.row_type.unwrap())));
            if let Some(array) = oids.array_type {
                assert!(expected.contains(&i64::from(array)));
            }
        }
    }
}

#[test]
fn type_membership_uses_the_selected_generation_after_rename_removal_and_rollback() {
    let mut snapshot = empty_catalog().snapshot().clone();
    add_user_types(&mut snapshot);
    add_relations(&mut snapshot, 1);
    let retained = CatalogReadView::new(snapshot.clone());
    let definition = snapshot
        .tables
        .remove(&RelationIdentity::new("public", "t0"))
        .unwrap();
    snapshot
        .tables
        .insert(RelationIdentity::new("other", "renamed"), definition);
    let renamed = CatalogReadView::new(snapshot.clone());
    snapshot.tables.clear();
    Arc::make_mut(&mut snapshot.definitions.domains).clear();
    Arc::make_mut(&mut snapshot.definitions.enums).clear();
    Arc::make_mut(&mut snapshot.definitions.composites).clear();
    let removed = CatalogReadView::new(snapshot);
    let before = TYPE_PROJECTION_BUILDS.get();
    for (catalog, occupied) in [
        (&retained, true),
        (&renamed, true),
        (&removed, false),
        (&retained, true),
    ] {
        for oid in [
            60_000, 60_001, 60_010, 60_011, 60_020, 60_021, 70_001, 70_002,
        ] {
            assert_eq!(
                catalog_oid_in_use(catalog, &resolution(), CatalogOidClass::Type, oid).unwrap(),
                occupied
            );
        }
    }
    assert_eq!(TYPE_PROJECTION_BUILDS.get(), before);
}

#[test]
fn recorded_relation_and_graph_arrays_occupy_only_the_type_oid_class() {
    use uqa_sql::catalog::graph_oids::{GraphCatalogOids, LabelCatalogOids};
    let mut snapshot = empty_catalog().snapshot().clone();
    add_relations(&mut snapshot, 0);
    let oids = |relation| RelationCatalogOids {
        relation,
        row_type: Some(relation + 1),
        array_type: Some(relation + 2),
        rule: None,
    };
    for (index, view) in Arc::make_mut(&mut snapshot.definitions.views)
        .values_mut()
        .enumerate()
    {
        view.definition.catalog_oids = Some(oids(80_000 + 10 * u32::try_from(index).unwrap()));
    }
    Arc::make_mut(&mut snapshot.definitions.foreign_tables)
        .values_mut()
        .next()
        .unwrap()
        .catalog_oids = Some(oids(80_020));
    snapshot.definitions.graph_catalog_oids = Arc::new(BTreeMap::from([(
        "graph".into(),
        GraphCatalogOids {
            namespace: 80_030,
            label_sequence: 80_031,
            labels: BTreeMap::from([(
                1,
                LabelCatalogOids {
                    sequence: 80_040,
                    relation: oids(80_050),
                    id_default: 80_060,
                    properties_default: 80_061,
                    not_null: BTreeMap::new(),
                    toast_table: 80_062,
                    toast_index: 80_063,
                    primary_key: None,
                    endpoint_indexes: None,
                    trigger: 80_064,
                },
            )]),
        },
    )]));
    let catalog = CatalogReadView::new(snapshot);
    let before = TYPE_PROJECTION_BUILDS.get();
    for relation in [80_000, 80_010, 80_020, 80_050] {
        for (oid, occupied) in [
            (relation, false),
            (relation + 1, true),
            (relation + 2, true),
        ] {
            assert_eq!(
                catalog_oid_in_use(&catalog, &resolution(), CatalogOidClass::Type, oid).unwrap(),
                occupied,
                "OID {oid}"
            );
        }
    }
    assert_eq!(TYPE_PROJECTION_BUILDS.get(), before);
}
