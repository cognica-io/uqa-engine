//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

#[test]
fn relation_descriptor_cache_retains_results_and_bounds_missing_identities() {
    let cache = RelationDescriptorCache::default();
    let builds = Cell::new(0);
    let first = cache
        .get_or_try_init(42, || {
            builds.set(builds.get() + 1);
            Ok(Some(Arc::new(CompositeTypeDescriptor {
                type_oid: 42,
                relation_oid: 43,
                attributes: Vec::new(),
                dropped: Vec::new(),
            })))
        })
        .unwrap()
        .unwrap();
    for _ in 0..32 {
        let hit = cache
            .get_or_try_init(42, || {
                builds.set(builds.get() + 1);
                Ok(None)
            })
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
    }
    assert_eq!(builds.get(), 1);
    for oid in 100..200 {
        for _ in 0..2 {
            assert!(cache
                .get_or_try_init(oid, || {
                    builds.set(builds.get() + 1);
                    Ok(None)
                })
                .unwrap()
                .is_none());
        }
    }
    assert_eq!(builds.get(), 101);
    let entries = cache.entries.lock();
    assert_eq!(entries.known.len(), 1);
    assert_eq!(entries.last_missing, Some(199));
}

#[test]
fn relation_descriptor_errors_and_new_generations_rebuild() {
    let original = crate::catalog::test_support::empty_catalog();
    let retained = original.clone();
    let replacement = CatalogReadView::new(original.snapshot().clone());
    assert!(original
        .relation_descriptors
        .get_or_try_init(7, || Err(SQLError::Internal("invalid attribute".into())))
        .is_err());
    let descriptor = original
        .relation_descriptors
        .get_or_try_init(7, || {
            Ok(Some(Arc::new(CompositeTypeDescriptor {
                type_oid: 7,
                relation_oid: 8,
                attributes: Vec::new(),
                dropped: Vec::new(),
            })))
        })
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &descriptor,
        &retained
            .relation_descriptors
            .get_or_try_init(7, || panic!("retained descriptor rebuilt"))
            .unwrap()
            .unwrap()
    ));
    assert!(replacement
        .relation_descriptors
        .get_or_try_init(7, || Ok(None))
        .unwrap()
        .is_none());
}
