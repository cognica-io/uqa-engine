//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::cache::RegtypeOutputCache;
use std::sync::atomic::{AtomicUsize, Ordering};

fn metadata(generation: usize) -> RegtypeOutputCatalog {
    RegtypeOutputCatalog {
        namespaces: BTreeMap::from([(1, format!("generation_{generation}"))]),
        classes: BTreeMap::new(),
        procs: BTreeMap::new(),
        proc_names_by_namespace: BTreeMap::new(),
        types: BTreeMap::new(),
        dependencies: crate::catalog::projection::DependencyCatalogCache::default(),
    }
}

#[test]
fn concurrent_output_readers_share_one_cold_metadata_build() {
    let cache = RegtypeOutputCache::default();
    let builds = AtomicUsize::new(0);
    let ready = std::sync::Barrier::new(8);
    let catalogs = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    cache
                        .get_or_try_init(|| Ok(metadata(builds.fetch_add(1, Ordering::SeqCst))))
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    for catalog in &catalogs {
        assert!(Arc::ptr_eq(catalog, &catalogs[0]));
    }
}

#[test]
fn output_build_can_invalidate_and_retries_before_publication() {
    let cache = RegtypeOutputCache::default();
    let builds = AtomicUsize::new(0);
    let catalog = cache
        .get_or_try_init(|| {
            let generation = builds.fetch_add(1, Ordering::SeqCst);
            if generation == 0 {
                cache.clear();
            }
            Ok(metadata(generation))
        })
        .unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(cache.revision(), 1);
    assert_eq!(catalog.namespaces[&1], "generation_1");
    assert!(Arc::ptr_eq(
        &catalog,
        &cache
            .get_or_try_init(|| panic!("warm metadata rebuilt"))
            .unwrap()
    ));
    cache.clear();
    let current = cache.get_or_try_init(|| Ok(metadata(2))).unwrap();
    assert!(!Arc::ptr_eq(&catalog, &current));
    assert_eq!(catalog.namespaces[&1], "generation_1");
    assert_eq!(current.namespaces[&1], "generation_2");
}

#[test]
fn failed_output_build_preserves_the_error_and_releases_initialization() {
    let cache = RegtypeOutputCache::default();
    let error = cache
        .get_or_try_init(|| {
            Err(SQLError::Routine {
                sqlstate: "57014".into(),
                message: "cancelled during metadata construction".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(!cache.is_populated());
    assert_eq!(
        cache
            .get_or_try_init(|| Ok(metadata(0)))
            .unwrap()
            .namespaces[&1],
        "generation_0"
    );
}
