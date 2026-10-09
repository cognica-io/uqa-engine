//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot, CatalogServices};
use std::sync::Arc;

mod fixtures;
mod lifecycle;
use fixtures::fixture;

fn work() -> [usize; 4] {
    [
        TRIGGER_COPIES.get(),
        TRIGGER_ADDRESSES.get(),
        RULE_ADDRESSES.get(),
        VIEW_ADDRESSES.get(),
    ]
}

#[test]
fn event_inquiries_share_trigger_collections_and_borrow_rule_definitions() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let resolution = CatalogServices::default().resolution;
        let before = work();
        let relation = RelationIdentity::new("public", "items");
        let rule = &catalog.snapshot().definitions.rules[&relation]["rule_000"];
        let view =
            &catalog.snapshot().definitions.views[&RelationIdentity::new("public", "view_000")];
        for _ in 0..4 {
            let alias = catalog.clone();
            let rows = catalog_triggers(&alias, &resolution).unwrap();
            assert_eq!(rows.len(), unrelated + 1);
            assert!(std::ptr::eq(
                trigger_by_oid(&alias, &resolution, 70_000)
                    .unwrap()
                    .unwrap(),
                &raw const rows[0].0
            ));
            assert!(trigger_by_oid(&alias, &resolution, -1).unwrap().is_none());
            assert!(std::ptr::eq(rule_by_oid(&alias, 80_000).unwrap(), rule));
            assert!(rule_by_oid(&alias, -1).is_none());
            assert!(std::ptr::eq(
                view_rule_by_oid(&alias, 90_003).unwrap().1,
                view
            ));
            assert!(view_rule_by_oid(&alias, -1).is_none());
            assert!(std::ptr::eq(view_by_oid(&alias, 90_000).unwrap(), view));
            assert!(view_by_oid(&alias, -1).is_none());
        }
        let after = work();
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|i| after[i] - before[i]),
            [unrelated + 1; 4]
        );
    }
}

#[test]
fn concurrent_event_inquiries_derive_each_collection_once() {
    let catalog = fixture(128);
    let resolution = CatalogServices::default().resolution;
    let ready = std::sync::Barrier::new(8);
    let totals = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = work();
                    assert!(trigger_by_oid(&catalog, &resolution, 70_000)
                        .unwrap()
                        .is_some());
                    assert!(rule_by_oid(&catalog, 80_000).is_some());
                    assert!(view_rule_by_oid(&catalog, 90_003).is_some());
                    let after = work();
                    std::array::from_fn::<_, 4, _>(|i| after[i] - before[i])
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .fold([0; 4], |sum, reads| {
                std::array::from_fn(|i| sum[i] + reads[i])
            })
    });
    assert_eq!(totals, [129; 4]);
}

#[test]
fn trigger_lookup_preserves_the_first_address_error_after_earlier_matches() {
    let catalog = fixture(2);
    let mut snapshot = catalog.snapshot().clone();
    let triggers = Arc::make_mut(&mut snapshot.definitions.triggers)
        .get_mut(&RelationIdentity::new("public", "items"))
        .unwrap();
    for trigger in triggers.values_mut() {
        trigger.definition.table = "public.missing".into();
    }
    triggers.get_mut("watch_001").unwrap().catalog_oid = None;
    let catalog = CatalogReadView::new(snapshot);
    let resolution = CatalogServices::default().resolution;
    let before = TRIGGER_ADDRESSES.get();
    assert_eq!(
        trigger_by_oid(&catalog, &resolution, 70_000)
            .unwrap()
            .unwrap()
            .definition
            .name,
        "watch_000"
    );
    for oid in [70_001, 70_002, -1] {
        let error = trigger_by_oid(&catalog, &resolution, oid).unwrap_err();
        assert!(matches!(error, SQLError::UnknownTable(ref name) if name == "public.missing"));
    }
    assert_eq!(TRIGGER_ADDRESSES.get() - before, 2);
    assert_eq!(catalog_triggers(&catalog, &resolution).unwrap().len(), 3);
}

#[test]
fn malformed_partition_collection_is_not_published() {
    let catalog = fixture(1);
    let mut snapshot = catalog.snapshot().clone();
    let table = snapshot
        .tables
        .get_mut(&RelationIdentity::new("public", "items"))
        .unwrap();
    Arc::make_mut(&mut table.hierarchy).partition_bound =
        Some(uqa_sql::ast::PartitionBound::Default);
    let invalid = CatalogReadView::new(snapshot);
    let resolution = CatalogServices::default().resolution;
    let before = TRIGGER_COPIES.get();
    for _ in 0..2 {
        let error = trigger_by_oid(&invalid, &resolution, 70_000).unwrap_err();
        assert!(error
            .to_string()
            .contains("partition `public.items` has no parent"));
    }
    assert_eq!(TRIGGER_COPIES.get() - before, 4);
}
