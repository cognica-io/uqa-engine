//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleBinding, RoleDefinition};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn namespace_capture_owns_exact_buffers_and_retains_bound_role_identity() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let bound = Arc::new(RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap());
    for role in [
        RoleReference::Bound(bound.clone()),
        RoleReference::Named("named".into()),
    ] {
        let resolution = RelationNameResolution::from_inputs_with_control(
            ["public", "pg_catalog", "public"],
            control.copy_text("pg_temp_123").unwrap(),
            true,
            &role,
            RelationLookupMode::Bound,
            &control,
        )
        .unwrap();
        assert_eq!(resolution.search_path, ["public", "pg_catalog", "public"]);
        assert!(resolution.temporary_namespace_allocated);
        assert_eq!(resolution.lookup_mode, RelationLookupMode::Bound);
        let role_bytes = match &resolution.current_user {
            RoleReference::Named(name) => name.capacity(),
            RoleReference::Bound(retained) => {
                assert!(Arc::ptr_eq(retained, &bound));
                0
            }
        };
        let bytes = resolution.search_path.capacity() * size_of::<String>()
            + resolution
                .search_path
                .iter()
                .map(String::capacity)
                .sum::<usize>()
            + resolution.temporary_schema.capacity()
            + role_bytes;
        assert_eq!(budget.used(), bytes);
        assert_eq!(resolution.reserved_bytes(), bytes);
        let copied = resolution.copy_with_control(&control).unwrap();
        assert_eq!(copied.search_path, resolution.search_path);
        drop((copied, resolution));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn namespace_capture_rejects_before_copy_and_releases_partial_work() {
    let name = "wide_namespace_".repeat(64 * 1024);
    let budget = MemoryBudget::new(512);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(7).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = RelationNameResolution::from_inputs_with_control(
            ["public", name.as_str()],
            control.copy_text("pg_temp_1").unwrap(),
            false,
            &RoleReference::Named("uqa".into()),
            RelationLookupMode::Dynamic,
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 7);
    for token in [&original, &invoking] {
        token.cancel();
        let ordinary = RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "uqa".into(),
            lookup_mode: RelationLookupMode::Dynamic,
        };
        assert_eq!(
            ordinary
                .copy_with_control(&control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 7);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
