//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn concurrent_compilation_and_json_restoration_share_one_immutable_revision() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let config = uqa_analysis::standard_analyzer("english");
    let descriptor = resolve(&config);
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let resources = resources.clone();
            let config = config.clone();
            let descriptor = descriptor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let compiled = if index % 2 == 0 {
                    resources.compile(&config).unwrap()
                } else {
                    resources.restore_json(descriptor.canonical_json()).unwrap()
                };
                assert_eq!(compiled.analyze("The cats and").unwrap(), ["cat"]);
                compiled
            })
        })
        .collect();
    let handles: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(handles
        .iter()
        .all(|handle| Arc::ptr_eq(handle, &handles[0])));
    assert_eq!(resources.cache_stats().analyzers, 1);
    assert_eq!(
        resources.cache_stats().descriptor_bytes,
        descriptor.canonical_json().len()
    );
    let default = config.compile().unwrap();
    assert!(Arc::ptr_eq(
        &default,
        &config
            .compile_with_resources(&AnalyzerResources::default())
            .unwrap()
    ));
}

#[test]
fn count_and_byte_eviction_preserve_live_handles_and_refresh_recency() {
    let keyword = keyword();
    let whitespace = Analyzer::default();
    let standard = uqa_analysis::standard_analyzer("english");
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_cached_analyzers: 2,
        ..AnalyzerLimits::default()
    });
    let first = resources.compile(&keyword).unwrap();
    let second = resources.compile(&whitespace).unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &resources.restore(first.descriptor().clone()).unwrap()
    ));
    let third = resources.compile(&standard).unwrap();
    assert_eq!(resources.cache_stats().analyzers, 2);
    assert_eq!(
        resources.cache_stats().descriptor_bytes,
        first.descriptor().canonical_json().len() + third.descriptor().canonical_json().len()
    );
    assert!(Arc::ptr_eq(&first, &resources.compile(&keyword).unwrap()));
    assert!(!Arc::ptr_eq(
        &second,
        &resources.compile(&whitespace).unwrap()
    ));
    assert_eq!(second.analyze("cats and").unwrap(), ["cats", "and"]);
    assert_eq!(third.analyze("cats and").unwrap(), ["cat"]);
    let maximum =
        first.descriptor().canonical_json().len() + second.descriptor().canonical_json().len() - 1;
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_cached_descriptor_bytes: maximum,
        ..AnalyzerLimits::default()
    });
    let first = resources.compile(&keyword).unwrap();
    let second = resources.compile(&whitespace).unwrap();
    assert_eq!(resources.cache_stats().analyzers, 1);
    assert_eq!(
        resources.cache_stats().descriptor_bytes,
        second.descriptor().canonical_json().len()
    );
    assert!(!Arc::ptr_eq(&first, &resources.compile(&keyword).unwrap()));
    assert_eq!(first.analyze("still valid").unwrap(), ["still valid"]);
}

#[test]
fn disabled_or_undersized_caches_return_valid_uncached_handles() {
    for limits in [
        AnalyzerLimits {
            max_cached_analyzers: 0,
            ..AnalyzerLimits::default()
        },
        AnalyzerLimits {
            max_cached_descriptor_bytes: 0,
            ..AnalyzerLimits::default()
        },
        AnalyzerLimits {
            max_cached_descriptor_bytes: 1,
            ..AnalyzerLimits::default()
        },
    ] {
        let resources = AnalyzerResources::new(limits);
        let first = resources.compile(&keyword()).unwrap();
        let second = resources.restore(first.descriptor().clone()).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.analyze("x").unwrap(), ["x"]);
        assert_eq!(resources.cache_stats().analyzers, 0);
        assert_eq!(resources.cache_stats().descriptor_bytes, 0);
    }
}
