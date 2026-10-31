//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::empty_catalog;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::resolution::RelationLookupMode;

fn namespace() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["public".into(), "selected".into()],
        temporary_schema: "pg_temp_71".into(),
        temporary_namespace_allocated: true,
        current_user: "owner".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    }
}

#[test]
fn captured_namespace_and_shared_scope_state_keep_independent_last_owner_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::with_catalog_with_control(
        empty_catalog(),
        namespace().copy_with_control(&control).unwrap(),
        &control,
    )
    .unwrap();
    scope.set_catalog_revision(10, 71);
    scope.set_reads_command_overlay(false);
    scope
        .set_relation_lookup_mode(RelationLookupMode::Bound)
        .unwrap();
    let mut child = None;
    let measured = allocation_counter::measure(|| {
        child = Some(scope.clone());
    });
    assert_eq!(measured.count_total, 0);
    let child = child.unwrap();
    assert_eq!(child.catalog_revision(), Some((10, 71)));
    assert!(!child.reads_command_overlay());
    assert_eq!(
        child.relation_lookup_mode(),
        Some(RelationLookupMode::Bound)
    );
    {
        let selected = scope.enter_relation_lookup_mode(false).unwrap();
        assert_eq!(
            selected.relation_lookup_mode(),
            Some(RelationLookupMode::Dynamic)
        );
        assert_eq!(
            child.relation_lookup_mode(),
            Some(RelationLookupMode::Bound)
        );
    }
    assert_eq!(
        scope.relation_name_resolution().unwrap().lookup_mode,
        RelationLookupMode::Bound
    );
    let counter = Arc::clone(&scope.next_scalar_subquery_arena);
    let counter_bytes = counter.reserved_bytes();
    let cache = Arc::clone(&scope.scalar_subquery_cache);
    let cache_bytes = cache.reserved_bytes();
    drop((scope, child));
    assert_eq!(budget.used(), counter_bytes + cache_bytes);
    drop(counter);
    assert_eq!(budget.used(), cache_bytes);
    drop(cache);
    assert_eq!(budget.used(), 0);
}

#[test]
fn transition_name_replacement_and_removal_preserve_exact_owner_lifetimes() {
    let rows = crate::spill::SpillBuffer::new(4096)
        .into_shared(crate::RowSchema::new(vec!["v".into()]))
        .unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::with_catalog_with_control(
        empty_catalog(),
        namespace().copy_with_control(&control).unwrap(),
        &control,
    )
    .unwrap();
    let baseline = budget.used();
    scope
        .insert_shared_with_control("new_rows", rows.clone(), &control)
        .unwrap();
    let held = budget.used();
    assert!(held > baseline);
    let occupied = budget.reserve(budget.available()).unwrap();
    let measured = allocation_counter::measure(|| {
        scope
            .insert_shared_with_control("new_rows", rows.clone(), &control)
            .unwrap();
    });
    assert_eq!(measured.count_total, 0);
    assert!(scope
        .insert_shared_with_control("another", rows.clone(), &control)
        .is_err());
    assert_eq!(scope.rows.len(), 1);
    drop(occupied);
    assert_eq!(budget.used(), held);
    let selected = scope.remove_materialized("new_rows").unwrap();
    assert_eq!(selected.row_schema(), rows.row_schema());
    assert_eq!(budget.used(), baseline);
    drop((selected, scope));
    assert_eq!(budget.used(), 0);
}

#[test]
fn scope_constructor_rejects_before_namespace_copy_and_releases_partial_shared_state() {
    let catalog = empty_catalog();
    let source = namespace();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for limit in [0, 64, 128, 256, 512] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = source.copy_with_control(&control).and_then(|namespace| {
            CteScope::<()>::with_catalog_with_control(catalog.clone(), namespace, &control)
        });
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let wide = RelationNameResolution {
        search_path: vec!["wide".repeat(128 * 1024)],
        ..source
    };
    let budget = MemoryBudget::new(1024);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            wide.copy_with_control(&control).err().unwrap().sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            CteScope::<()>::empty_with_control(&control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
