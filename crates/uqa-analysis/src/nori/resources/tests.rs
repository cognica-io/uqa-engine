//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::nori::{frame, tests::fixtures};

fn bytes(cost: i16) -> Arc<[u8]> {
    let mut sections = fixtures::sections();
    sections[3].bytes[8..10].copy_from_slice(&cost.to_le_bytes());
    frame::encode(&sections, DictionaryLimits::default())
        .unwrap()
        .into()
}

fn artifact(bytes: Arc<[u8]>) -> DictionaryArtifact {
    DictionaryArtifact {
        sha256: ResourceHash::of(&bytes),
        bytes: DictionaryBytes::Shared(bytes),
    }
}

fn name(value: &str) -> DictionaryRequest {
    DictionaryRequest::Name(value.into())
}

fn supplied(bytes: Arc<[u8]>, limits: ResourceLimits) -> NoriResources {
    NoriResources::with_resolver(
        Arc::new(move |_: &DictionaryRequest| Ok(Some(artifact(bytes.clone())))),
        limits,
    )
}

#[test]
fn content_resolution_shares_validated_handles_and_never_caches_a_mutable_alias() {
    let current = Arc::new(Mutex::new(bytes(-10)));
    let calls = Arc::new(AtomicUsize::new(0));
    let resources = NoriResources::with_resolver(
        Arc::new({
            let current = current.clone();
            let calls = calls.clone();
            move |_: &DictionaryRequest| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Some(artifact(current.lock().clone())))
            }
        }),
        ResourceLimits::default(),
    );
    let first = resources.load(&name("alias")).unwrap();
    let second = resources.clone().load(&name("another-alias")).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(first.model(), second.model()));
    let exact = DictionaryRequest::Sha256(first.sha256());
    assert!(Arc::ptr_eq(&first, &resources.load(&exact).unwrap()));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    *current.lock() = bytes(123);
    let replacement = resources.load(&name("alias")).unwrap();
    assert_ne!(replacement.model().id(), first.model().id());
    assert_eq!(first.model().connection_cost(0, 0), Some(-10));
    assert_eq!(replacement.model().connection_cost(0, 0), Some(123));
    assert!(Arc::ptr_eq(&first, &resources.load(&exact).unwrap()));
    assert_eq!(ResourceHash::of(first.bytes()), first.sha256());
}

#[test]
fn concurrent_content_misses_publish_one_shared_model() {
    let resources = supplied(bytes(-10), ResourceLimits::default());
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let resources = resources.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                resources.load(&name("alias")).unwrap()
            })
        })
        .collect();
    let resolved: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(resolved
        .iter()
        .all(|entry| Arc::ptr_eq(entry, &resolved[0])));
    assert_eq!(resources.cache_stats().dictionaries, 1);
}

#[test]
fn resolver_callbacks_can_resolve_another_resource_without_holding_the_cache_lock() {
    let owner = Arc::new(Mutex::new(None::<NoriResources>));
    let resources = NoriResources::with_resolver(
        Arc::new({
            let owner = owner.clone();
            let bytes = bytes(-10);
            move |request: &DictionaryRequest| {
                if request == &name("outer") {
                    let resources = owner.lock().clone().unwrap();
                    resources.load(&name("inner"))?;
                }
                Ok(Some(artifact(bytes.clone())))
            }
        }),
        ResourceLimits::default(),
    );
    *owner.lock() = Some(resources.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn({
        let resources = resources.clone();
        move || sender.send(resources.load(&name("outer"))).unwrap()
    });
    let outer = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    owner.lock().take();
    let inner = resources.load(&name("inner")).unwrap();
    assert!(Arc::ptr_eq(&outer, &inner));
}

#[test]
fn resource_failures_preserve_empty_caches_and_allow_recovery() {
    let valid = bytes(-10);
    let current = Arc::new(Mutex::new((ResourceHash::of(b"wrong"), valid.clone())));
    let resources = NoriResources::with_resolver(
        Arc::new({
            let current = current.clone();
            move |request: &DictionaryRequest| {
                if request == &name("missing") {
                    return Ok(None);
                }
                let (sha256, bytes) = current.lock().clone();
                Ok(Some(DictionaryArtifact {
                    sha256,
                    bytes: DictionaryBytes::Shared(bytes),
                }))
            }
        }),
        ResourceLimits::default(),
    );
    assert!(matches!(
        resources.load(&name("missing")),
        Err(DictionaryError::ResourceMissing(_))
    ));
    assert!(matches!(
        resources.load(&name("alias")),
        Err(DictionaryError::ResourceHashMismatch { .. })
    ));
    *current.lock() = (ResourceHash::of(&valid), valid.clone());
    assert!(matches!(
        resources.load(&DictionaryRequest::Sha256(ResourceHash::of(b"requested"))),
        Err(DictionaryError::ResourceHashMismatch { .. })
    ));
    let mut corrupt = valid.to_vec();
    corrupt[0] ^= 1;
    *current.lock() = (ResourceHash::of(&corrupt), corrupt.into());
    assert!(matches!(
        resources.load(&name("alias")),
        Err(DictionaryError::Invalid { .. })
    ));
    assert_eq!(resources.cache_stats().dictionaries, 0);
    *current.lock() = (ResourceHash::of(&valid), valid);
    assert!(resources.load(&name("alias")).is_ok());
    assert_eq!(resources.cache_stats().dictionaries, 1);
}

#[test]
fn bounded_caches_evict_least_recently_used_ownership_without_invalidating_handles() {
    let alternatives = [bytes(-10), bytes(1), bytes(2)];
    let resources = NoriResources::with_resolver(
        Arc::new({
            let alternatives = alternatives.clone();
            move |request: &DictionaryRequest| {
                let DictionaryRequest::Name(name) = request else {
                    return Ok(None);
                };
                Ok(Some(artifact(
                    alternatives[name.parse::<usize>().unwrap()].clone(),
                )))
            }
        }),
        ResourceLimits::default(),
    );
    let first = resources.load(&name("0")).unwrap();
    let second = resources.load(&name("1")).unwrap();
    resources
        .load(&DictionaryRequest::Sha256(first.sha256()))
        .unwrap();
    resources.load(&name("2")).unwrap();
    assert_eq!(resources.cache_stats().dictionaries, 2);
    assert!(Arc::ptr_eq(&first, &resources.load(&name("0")).unwrap()));
    let reloaded = resources.load(&name("1")).unwrap();
    assert!(!Arc::ptr_eq(&second, &reloaded));
    assert_eq!(second.model().id(), reloaded.model().id());
    assert_eq!(second.model().connection_cost(0, 0), Some(1));

    let uncached = supplied(
        alternatives[0].clone(),
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..ResourceLimits::default()
        },
    );
    let first = uncached.load(&name("alias")).unwrap();
    let second = uncached.load(&name("alias")).unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(uncached.cache_stats().dictionaries, 0);
}

#[test]
fn resource_limits_are_applied_before_hashing_and_before_decoding_publication() {
    let valid = bytes(-10);
    for dictionary in [
        DictionaryLimits {
            max_encoded_bytes: valid.len() - 1,
            ..DictionaryLimits::default()
        },
        DictionaryLimits {
            max_decoded_bytes: 1,
            ..DictionaryLimits::default()
        },
    ] {
        let resources = supplied(
            valid.clone(),
            ResourceLimits {
                dictionary,
                ..ResourceLimits::default()
            },
        );
        assert!(matches!(
            resources.load(&name("alias")),
            Err(DictionaryError::Limit { .. })
        ));
        assert_eq!(resources.cache_stats().dictionaries, 0);
    }
}

#[test]
fn cache_byte_budgets_bound_retention_independently_of_entry_counts() {
    let first = bytes(-10);
    let second = bytes(1);
    let budget = first.len().max(second.len());
    let resources = NoriResources::with_resolver(
        Arc::new({
            let first = first.clone();
            let second = second.clone();
            move |request: &DictionaryRequest| {
                Ok(Some(artifact(if request == &name("first") {
                    first.clone()
                } else {
                    second.clone()
                })))
            }
        }),
        ResourceLimits {
            max_cached_encoded_bytes: budget,
            max_cached_user_source_bytes: 5,
            ..ResourceLimits::default()
        },
    );
    let previous = resources.load(&name("first")).unwrap();
    let current = resources.load(&name("second")).unwrap();
    assert_eq!(resources.cache_stats().dictionaries, 1);
    assert_eq!(
        resources.cache_stats().dictionary_encoded_bytes,
        second.len()
    );
    assert!(!Arc::ptr_eq(
        &previous,
        &resources.load(&name("first")).unwrap()
    ));
    assert_eq!(previous.model().connection_cost(0, 0), Some(-10));
    assert_eq!(current.model().connection_cost(0, 0), Some(1));
    let short = resources.compile_user("# a", previous.model()).unwrap();
    resources.compile_user("# b", previous.model()).unwrap();
    assert_eq!(resources.cache_stats().user_dictionaries, 1);
    assert_eq!(resources.cache_stats().user_source_bytes, 3);
    assert!(!Arc::ptr_eq(
        &short,
        &resources.compile_user("# a", previous.model()).unwrap()
    ));
    let oversized = resources
        .compile_user("# beyond cache", previous.model())
        .unwrap();
    assert!(!Arc::ptr_eq(
        &oversized,
        &resources
            .compile_user(oversized.source(), previous.model())
            .unwrap()
    ));
    assert_eq!(resources.cache_stats().user_source_bytes, 3);
    let uncached = supplied(
        first,
        ResourceLimits {
            max_cached_encoded_bytes: 0,
            ..ResourceLimits::default()
        },
    );
    assert!(uncached.load(&name("first")).is_ok());
    assert_eq!(uncached.cache_stats().dictionary_encoded_bytes, 0);
    assert_eq!(uncached.cache_stats().dictionaries, 0);
}

#[test]
fn empty_user_rule_snapshots_retain_exact_source_and_model_identity_under_eviction() {
    let resources = supplied(
        bytes(-10),
        ResourceLimits {
            max_cached_user_dictionaries: 1,
            user_dictionary: UserDictionaryLimits {
                max_bytes: 16,
                ..UserDictionaryLimits::default()
            },
            ..ResourceLimits::default()
        },
    );
    let first = resources.load(&name("alias")).unwrap();
    let changed = NoriDictionary::from_bytes(&bytes(1), DictionaryLimits::default()).unwrap();
    let comments = resources.compile_user("# first\n", first.model()).unwrap();
    assert!(comments.dictionary().is_none());
    assert_eq!(comments.source(), "# first\n");
    assert!(Arc::ptr_eq(
        &comments,
        &resources.compile_user("# first\n", first.model()).unwrap()
    ));
    assert!(matches!(
        resources.compile_user(&" ".repeat(17), first.model()),
        Err(DictionaryError::Limit { .. })
    ));
    assert_eq!(resources.cache_stats().user_dictionaries, 1);
    assert!(Arc::ptr_eq(
        &comments,
        &resources.compile_user("# first\n", first.model()).unwrap()
    ));
    let other_model = resources.compile_user("# first\n", &changed).unwrap();
    assert!(!Arc::ptr_eq(&comments, &other_model));
    assert_ne!(comments.model_id(), other_model.model_id());
    assert_eq!(comments.sha256(), other_model.sha256());
    let empty = resources.compile_user("", first.model()).unwrap();
    assert_ne!(empty.sha256(), comments.sha256());
    assert_eq!(empty.source(), "");
    assert_eq!(resources.cache_stats().user_dictionaries, 1);
    assert_eq!(comments.source(), "# first\n");
}

#[test]
fn hashes_use_canonical_hex_and_reject_invalid_external_identity() {
    let hash = ResourceHash::of(b"abc");
    assert_eq!(
        hash.to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hash.to_string()
            .to_ascii_uppercase()
            .parse::<ResourceHash>()
            .unwrap(),
        hash
    );
    let request = DictionaryRequest::Sha256(hash);
    let serialized = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::from_str::<DictionaryRequest>(&serialized).unwrap(),
        request
    );
    for invalid in [
        String::new(),
        "0".repeat(63),
        "0".repeat(65),
        "g".repeat(64),
        "🙂".repeat(16),
    ] {
        assert!(invalid.parse::<ResourceHash>().is_err());
    }
}
