//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    test_support::{empty_catalog, table_snapshot, CatalogServices},
};
use std::sync::Arc;
use uqa_core::{catalog_identity::CatalogObjectIdentity, Value};
use uqa_sql::{ast::ColumnDef, catalog::index::IndexCatalogIdentity, ColumnType};

fn index_name(position: usize) -> RelationIdentity {
    RelationIdentity::new("public", format!("item_idx_{position:03}"))
}

fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(
        "uqa".into(),
        uqa_sql::catalog::roles::RoleDefinition::bootstrap(),
    )]));
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot(
            [1; 16],
            vec![ColumnDef::nullable("id", ColumnType::Integer)],
            uqa_sql::ast::TableConstraintSet::default(),
        ),
    );
    for position in 0..=unrelated {
        let definition = super::super::IndexDefinition {
            catalog: Some(IndexCatalogIdentity {
                identity: CatalogObjectIdentity {
                    object_id: (position as u128 + 1).to_le_bytes(),
                    oid: 70_000 + i64::try_from(position).unwrap(),
                },
                table_object_id: [1; 16],
                physical_key: format!("physical:{position}"),
            }),
            ..super::super::IndexDefinition::default()
        };
        Arc::make_mut(&mut snapshot.definitions.catalog_indexes).insert(
            index_name(position),
            uqa_storage::CatalogIndexRow {
                relation: index_name(position),
                table_name: "public.items".into(),
                index_type: "btree".into(),
                columns_json: r#"["id"]"#.into(),
                parameters_json: "{}".into(),
                definition_json: Some(serde_json::to_string(&definition).unwrap()),
            },
        );
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn index_inquiries_borrow_one_validated_collection() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let mut services = CatalogServices::default();
        services
            .resolution
            .set_lookup_mode(crate::catalog::RelationLookupMode::Dynamic);
        let output = RegtypeOutputCache::default();
        let context = services.context(&catalog, &output);
        let before = DECODED.get();
        let selected = catalog_index_by_oid(&catalog, 70_000).unwrap().unwrap();
        for _ in 0..4 {
            let alias = catalog.clone();
            let all = catalog_index_relations(&alias).unwrap();
            assert_eq!(all.len(), unrelated + 1);
            assert!(std::ptr::eq(selected, all.first().unwrap()));
            assert!(std::ptr::eq(
                selected,
                catalog_index_by_name(&alias, &index_name(0))
                    .unwrap()
                    .unwrap()
            ));
            assert!(catalog_index_by_oid(&alias, -1).unwrap().is_none());
            assert_eq!(
                crate::catalog::projection::pg_get_indexdef_value(&context, &[Value::Int(70_000)])
                    .unwrap(),
                Value::Str("CREATE INDEX item_idx_000 ON public.items USING btree (id)".into())
            );
            assert_eq!(
                crate::catalog::projection::pg_get_indexdef_value(&context, &[Value::Int(-1)])
                    .unwrap(),
                Value::Null
            );
        }
        assert_eq!(DECODED.get() - before, unrelated + 1);
    }
}

#[test]
fn concurrent_index_readers_decode_the_catalog_once() {
    let catalog = fixture(128);
    let ready = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = DECODED.get();
                    let index = catalog_index_by_oid(&catalog, 70_000).unwrap().unwrap();
                    (index, DECODED.get() - before)
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().map(|(_, decoded)| decoded).sum::<usize>(),
        129
    );
    for (index, _) in &results {
        assert!(std::ptr::eq(*index, results[0].0));
    }
}

#[test]
fn retained_index_collections_survive_rename_and_removal() {
    let catalog = fixture(1);
    let original = catalog_index_by_oid(&catalog, 70_000).unwrap().unwrap();
    let renamed = RelationIdentity::new("public", "renamed");
    let mut snapshot = catalog.snapshot().clone();
    let rows = Arc::make_mut(&mut snapshot.definitions.catalog_indexes);
    let mut row = rows.remove(&index_name(0)).unwrap();
    row.relation = renamed.clone();
    rows.insert(renamed.clone(), row);
    let current = CatalogReadView::new(snapshot.clone());
    let selected = catalog_index_by_oid(&current, 70_000).unwrap().unwrap();
    assert_eq!(selected.relation, renamed);
    assert_eq!(selected.definition, original.definition);
    assert!(catalog_index_by_name(&current, &index_name(0))
        .unwrap()
        .is_none());
    Arc::make_mut(&mut snapshot.definitions.catalog_indexes).remove(&renamed);
    let removed = CatalogReadView::new(snapshot);
    assert!(catalog_index_by_oid(&removed, 70_000).unwrap().is_none());
    assert_eq!(original.relation, index_name(0));
    assert!(std::ptr::eq(
        original,
        catalog_index_by_oid(&catalog, 70_000).unwrap().unwrap()
    ));
}

fn change_definition(
    snapshot: &mut crate::catalog::CatalogReadSnapshot,
    position: usize,
    change: impl FnOnce(&mut super::super::IndexDefinition),
) {
    let row = Arc::make_mut(&mut snapshot.definitions.catalog_indexes)
        .get_mut(&index_name(position))
        .unwrap();
    let mut definition = super::super::index_definition(row).unwrap();
    change(&mut definition);
    row.definition_json = Some(serde_json::to_string(&definition).unwrap());
}

#[test]
fn index_cache_keeps_complete_validation_order_and_retries_errors() {
    let catalog = fixture(1);
    let mut snapshot = catalog.snapshot().clone();
    change_definition(&mut snapshot, 0, |definition| {
        definition.catalog.as_mut().unwrap().table_object_id = [9; 16];
    });
    change_definition(&mut snapshot, 1, |definition| definition.catalog = None);
    let invalid = CatalogReadView::new(snapshot);
    let before = DECODED.get();
    for _ in 0..2 {
        let error = catalog_index_by_oid(&invalid, 70_000).unwrap_err();
        assert!(error
            .to_string()
            .contains("item_idx_001` has no catalog identity"));
    }
    assert_eq!(DECODED.get() - before, 4);
    for (case, expected) in [
        (
            0,
            "invalid index catalog identity or indexed table incarnation",
        ),
        (1, "has no owning constraint"),
        (2, "has no parent index"),
    ] {
        let mut snapshot = catalog.snapshot().clone();
        change_definition(&mut snapshot, 0, |definition| match case {
            0 => definition.catalog.as_mut().unwrap().table_object_id = [9; 16],
            1 => definition.relationships.owning_constraint = Some([9; 16]),
            _ => definition.relationships.parent_index = Some([9; 16]),
        });
        let invalid = CatalogReadView::new(snapshot);
        assert!(catalog_index_by_oid(&invalid, -1)
            .unwrap_err()
            .to_string()
            .contains(expected));
    }
    assert_eq!(
        catalog_index_by_oid(&catalog, 70_000)
            .unwrap()
            .unwrap()
            .oid(),
        70_000
    );
}
