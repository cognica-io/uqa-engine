//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{cache::RegtypeOutputCache, test_support::CatalogServices};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn type_oid_selection_copies_only_the_matching_descriptor_and_keeps_exact_lifetime() {
    let catalog = super::super::tests::fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let token = CancellationToken::new();
    let _ = CATALOG_NAMED_TYPES.len();
    for oid in [20001, 20002, 13221, u32::MAX] {
        let expected = super::super::resolve_catalog_user_type_by_oid(&context, oid);
        let memory = MemoryBudget::new(8192);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output =
                resolve_catalog_user_type_by_oid_with_control(&context, oid, &control).unwrap();
        });
        assert_eq!(output.as_deref(), expected.as_ref());
        assert_eq!(allocations.bytes_current, memory.used() as i64);
        assert_eq!(
            memory.used(),
            output.as_ref().map_or(0, Produced::reserved_bytes)
        );
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    let zero = MemoryBudget::new(0);
    let control = ProductionControl::new(&zero, &token, &token);
    assert_eq!(
        resolve_catalog_user_type_by_oid_with_control(&context, 20001, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    let allocations = allocation_counter::measure(|| {
        assert!(
            resolve_catalog_user_type_by_oid_with_control(&context, u32::MAX, &control)
                .unwrap()
                .is_none()
        );
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(zero.used(), 0);
}

#[test]
fn type_oid_selection_observes_both_cancellation_owners_before_descriptor_production() {
    let catalog = super::super::tests::fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(8192);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert_eq!(
            resolve_catalog_user_type_by_oid_with_control(&context, 20001, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
