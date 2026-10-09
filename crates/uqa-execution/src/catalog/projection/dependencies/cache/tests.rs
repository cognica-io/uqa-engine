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
use uqa_core::RelationIdentity;

fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(std::collections::BTreeMap::from([(
        "uqa".into(),
        uqa_sql::catalog::roles::RoleDefinition::bootstrap(),
    )]));
    for index in 0..=unrelated {
        snapshot.tables.insert(
            RelationIdentity::new("public", format!("item_{index}")),
            table_snapshot(
                (index as u128 + 1).to_le_bytes(),
                Vec::new(),
                uqa_sql::ast::TableConstraintSet::default(),
            ),
        );
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn dependency_sources_and_descriptions_share_one_retained_derivation() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let services = CatalogServices::default();
        let output = RegtypeOutputCache::default();
        let context = services.context(&catalog, &output);
        let expected =
            CatalogDependencies::build(&context, &catalog, &services.resolution).unwrap();
        let before = BUILDS.get();
        for _ in 0..4 {
            let alias = catalog.clone();
            for (name, rows) in [
                ("pg_depend", expected.depend_rows()),
                ("pg_shdepend", expected.shared_depend_rows()),
            ] {
                assert_eq!(
                    crate::catalog::projection::build_info_schema_rows(
                        &context,
                        &alias,
                        &services.resolution,
                        &services,
                        &format!("pg_catalog.{name}")
                    )
                    .unwrap()
                    .unwrap(),
                    rows,
                );
            }
            let selected = retained_dependencies(&context, &alias, &services.resolution).unwrap();
            let described =
                crate::catalog::projection::regtypes::catalog_dependencies(&context).unwrap();
            assert!(Arc::ptr_eq(&selected, &described));
        }
        assert_eq!(BUILDS.get() - before, 1);
    }
}

#[test]
fn description_metadata_reuses_dependencies_until_output_invalidation() {
    let catalog = fixture(1);
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let before = BUILDS.get();
    let described = crate::catalog::projection::regtypes::catalog_dependencies(&context).unwrap();
    assert!(Arc::ptr_eq(
        &described,
        &retained_dependencies(&context, &catalog, &services.resolution).unwrap()
    ));
    for _ in 0..4 {
        let next_query = CatalogReadView::new(catalog.snapshot().clone());
        let context = services.context(&next_query, &output);
        assert!(Arc::ptr_eq(
            &described,
            &crate::catalog::projection::regtypes::catalog_dependencies(&context).unwrap()
        ));
    }
    assert_eq!(BUILDS.get() - before, 1);
    let original = RelationIdentity::new("public", "item_0");
    let renamed = RelationIdentity::new("public", "renamed");
    let mut snapshot = catalog.snapshot().clone();
    let table = snapshot.tables.remove(&original).unwrap();
    snapshot.tables.insert(renamed.clone(), table);
    let current = CatalogReadView::new(snapshot);
    output.clear();
    let context = services.context(&current, &output);
    let new = crate::catalog::projection::regtypes::catalog_dependencies(&context).unwrap();
    assert_eq!(BUILDS.get() - before, 2);
    assert!(!Arc::ptr_eq(&described, &new));
    assert_eq!(
        described.relation_address(&original, None),
        new.relation_address(&renamed, None)
    );
    assert!(new.relation_address(&original, None).is_none());
}

#[test]
fn concurrent_dependency_readers_initialize_the_generation_once() {
    let catalog = fixture(128);
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let ready = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = BUILDS.get();
                    let dependencies =
                        retained_dependencies(&context, &catalog, &services.resolution).unwrap();
                    (dependencies, BUILDS.get() - before)
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().map(|(_, builds)| builds).sum::<usize>(), 1);
    for (dependencies, _) in &results {
        assert!(Arc::ptr_eq(dependencies, &results[0].0));
    }
}

#[test]
fn dependency_generations_preserve_renames_removal_and_fresh_ddl_builds() {
    let catalog = fixture(1);
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let original = RelationIdentity::new("public", "item_0");
    let renamed = RelationIdentity::new("public", "renamed");
    let dependencies = retained_dependencies(&context, &catalog, &services.resolution).unwrap();
    let address = dependencies.relation_address(&original, None).unwrap();
    let mut snapshot = catalog.snapshot().clone();
    let table = snapshot.tables.remove(&original).unwrap();
    snapshot.tables.insert(renamed.clone(), table);
    let current = CatalogReadView::new(snapshot.clone());
    // The helper must bind nested type inquiry to the supplied view, even if its caller retained an older source.
    let new = retained_dependencies(&context, &current, &services.resolution).unwrap();
    assert_eq!(new.relation_address(&renamed, None), Some(address));
    assert!(new.relation_address(&original, None).is_none());
    assert!(!Arc::ptr_eq(&dependencies, &new));
    snapshot.tables.remove(&renamed);
    let removed = CatalogReadView::new(snapshot);
    assert!(
        retained_dependencies(&context, &removed, &services.resolution)
            .unwrap()
            .relation_address(&renamed, None)
            .is_none()
    );
    assert_eq!(
        dependencies.relation_address(&original, None),
        Some(address)
    );
    assert!(Arc::ptr_eq(
        &dependencies,
        &retained_dependencies(&context, &catalog, &services.resolution).unwrap()
    ));
    let before = BUILDS.get();
    for _ in 0..2 {
        let fresh = CatalogDependencies::build(&context, &catalog, &services.resolution).unwrap();
        assert_eq!(fresh.relation_address(&original, None), Some(address));
    }
    assert_eq!(BUILDS.get() - before, 2);
}

#[test]
fn failed_dependency_derivation_does_not_publish_partial_metadata() {
    let catalog = fixture(0);
    let mut snapshot = catalog.snapshot().clone();
    let relation = RelationIdentity::new("public", "malformed");
    Arc::make_mut(&mut snapshot.definitions.catalog_indexes).insert(
        relation.clone(),
        uqa_storage::CatalogIndexRow {
            relation,
            table_name: "public.item_0".into(),
            index_type: "btree".into(),
            columns_json: "[]".into(),
            parameters_json: "{}".into(),
            definition_json: None,
        },
    );
    let invalid = CatalogReadView::new(snapshot);
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    let context = services.context(&invalid, &output);
    let before = BUILDS.get();
    for _ in 0..2 {
        assert!(
            retained_dependencies(&context, &invalid, &services.resolution)
                .unwrap_err()
                .to_string()
                .contains("has no catalog identity")
        );
    }
    assert_eq!(BUILDS.get() - before, 2);
    retained_dependencies(&context, &catalog, &services.resolution).unwrap();
}
