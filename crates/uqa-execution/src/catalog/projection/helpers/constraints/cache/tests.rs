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
use uqa_core::{
    catalog_identity::CatalogObjectIdentity, catalog_role::RoleIdentity, RelationIdentity, Value,
};
use uqa_sql::{ast::TableConstraintSet, catalog::domain::StoredDomain, Statement};

mod domains;

fn table_name(position: usize) -> RelationIdentity {
    RelationIdentity::new("public", format!("items_{position:03}"))
}

fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(
        "uqa".into(),
        uqa_sql::catalog::roles::RoleDefinition::bootstrap(),
    )]));
    for position in 0..=unrelated {
        let Statement::CreateTable(mut table) =
            uqa_sql::compile("CREATE TABLE items(id integer, CONSTRAINT positive CHECK(id > 0))")
                .unwrap()
                .remove(0)
        else {
            panic!("table");
        };
        let object_id = (position as u128 + 1).to_le_bytes();
        table.checks[0].object_id = Some(object_id);
        table.checks[0].catalog_oid = Some(70_000 + i64::try_from(position).unwrap());
        snapshot.tables.insert(
            table_name(position),
            table_snapshot(
                object_id,
                table.columns,
                TableConstraintSet {
                    checks: table.checks,
                    ..TableConstraintSet::default()
                },
            ),
        );
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn constraint_inquiries_borrow_one_validated_collection() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let services = CatalogServices::default();
        let resolution = &services.resolution;
        let output = RegtypeOutputCache::default();
        let context = services.context(&catalog, &output);
        let before = TABLE_READS.get();
        let selected = constraint_catalog_row_by_oid(&catalog, resolution, 70_000)
            .unwrap()
            .unwrap();
        for _ in 0..4 {
            let alias = catalog.clone();
            let all = constraint_catalog_rows(&alias, resolution).unwrap();
            assert_eq!(all.len(), unrelated + 1);
            assert!(std::ptr::eq(selected, all.first().unwrap()));
            assert!(std::ptr::eq(
                selected,
                constraint_catalog_row_by_oid(&alias, resolution, 70_000)
                    .unwrap()
                    .unwrap()
            ));
            assert!(constraint_catalog_row_by_oid(&alias, resolution, -1)
                .unwrap()
                .is_none());
            assert_eq!(
                crate::catalog::projection::pg_get_constraintdef_value(
                    &context,
                    &[Value::Int(70_000)]
                )
                .unwrap(),
                Value::Str("CHECK ((id > 0))".into())
            );
            assert_eq!(
                crate::catalog::projection::pg_get_constraintdef_value(&context, &[Value::Int(-1)])
                    .unwrap(),
                Value::Null
            );
        }
        assert_eq!(TABLE_READS.get() - before, unrelated + 1);
    }
}

#[test]
fn concurrent_constraint_readers_collect_the_catalog_once() {
    let catalog = fixture(128);
    let resolution = CatalogServices::default().resolution;
    let ready = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = TABLE_READS.get();
                    let row = constraint_catalog_row_by_oid(&catalog, &resolution, 70_000)
                        .unwrap()
                        .unwrap();
                    (row, TABLE_READS.get() - before)
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().map(|(_, reads)| reads).sum::<usize>(), 129);
    for (row, _) in &results {
        assert!(std::ptr::eq(*row, results[0].0));
    }
}

#[test]
fn retained_constraint_definitions_survive_rename_and_removal() {
    let catalog = fixture(1);
    let resolution = CatalogServices::default().resolution;
    let original = constraint_catalog_row_by_oid(&catalog, &resolution, 70_000)
        .unwrap()
        .unwrap();
    let mut snapshot = catalog.snapshot().clone();
    let table = snapshot.tables.get_mut(&table_name(0)).unwrap();
    Arc::make_mut(&mut table.checks)[0].name = Some("renamed".into());
    let current = CatalogReadView::new(snapshot.clone());
    let selected = constraint_catalog_row_by_oid(&current, &resolution, 70_000)
        .unwrap()
        .unwrap();
    assert_eq!(selected.name, "renamed");
    assert_eq!(selected.expression, original.expression);
    snapshot.tables.remove(&table_name(0));
    let removed = CatalogReadView::new(snapshot);
    assert!(constraint_catalog_row_by_oid(&removed, &resolution, 70_000)
        .unwrap()
        .is_none());
    assert_eq!(original.name, "positive");
    assert!(std::ptr::eq(
        original,
        constraint_catalog_row_by_oid(&catalog, &resolution, 70_000)
            .unwrap()
            .unwrap()
    ));
}

#[test]
fn constraint_cache_keeps_complete_validation_order_and_retries_errors() {
    let catalog = fixture(1);
    let resolution = CatalogServices::default().resolution;
    let mut snapshot = catalog.snapshot().clone();
    for table in snapshot.tables.values_mut() {
        Arc::make_mut(&mut table.checks)[0].name = None;
    }
    let invalid = CatalogReadView::new(snapshot.clone());
    let error = constraint_catalog_row_by_oid(&invalid, &resolution, 70_000).unwrap_err();
    assert!(error.to_string().contains("public.items_000` has no name"));
    Arc::make_mut(&mut snapshot.tables.get_mut(&table_name(0)).unwrap().checks)[0].name =
        Some("positive".into());
    let invalid = CatalogReadView::new(snapshot);
    let before = TABLE_READS.get();
    for _ in 0..2 {
        // Even an existing first-row OID must report the later malformed declaration.
        let error = constraint_catalog_row_by_oid(&invalid, &resolution, 70_000).unwrap_err();
        assert!(error.to_string().contains("public.items_001` has no name"));
    }
    assert_eq!(TABLE_READS.get() - before, 4);
    assert_eq!(
        constraint_catalog_rows(&catalog, &resolution)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn duplicate_constraint_oid_selects_the_first_validated_row() {
    let catalog = fixture(1);
    let mut snapshot = catalog.snapshot().clone();
    Arc::make_mut(&mut snapshot.tables.get_mut(&table_name(1)).unwrap().checks)[0].catalog_oid =
        Some(70_000);
    let catalog = CatalogReadView::new(snapshot);
    let resolution = CatalogServices::default().resolution;
    assert_eq!(
        constraint_catalog_row_by_oid(&catalog, &resolution, 70_000)
            .unwrap()
            .unwrap()
            .table,
        "items_000"
    );
}
