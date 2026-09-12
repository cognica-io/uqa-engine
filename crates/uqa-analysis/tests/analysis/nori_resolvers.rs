//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_analysis::nori::{
    DictionaryError, DictionaryRequest, KoreanTokenizer, NoriOptions, NoriResources,
    ResourceLimits, UserDictionaryLimits,
};

use super::nori_resources::model;

#[test]
fn default_resources_resolve_exact_shipped_bytes_without_copying_or_fallback() {
    let resources = NoriResources::default();
    let dictionary = resources.load_default().unwrap();
    assert_eq!(
        dictionary.sha256().to_string(),
        uqa_nori_data::BUNDLE_SHA256
    );
    assert_eq!(
        dictionary.model().id().to_string(),
        uqa_nori_data::DICTIONARY_ID
    );
    assert_eq!(dictionary.bytes().as_ptr(), uqa_nori_data::BUNDLE.as_ptr());
    let exact = NoriResources::default()
        .load(&DictionaryRequest::Sha256(dictionary.sha256()))
        .unwrap();
    assert!(Arc::ptr_eq(&dictionary, &exact));
    assert!(Arc::ptr_eq(dictionary.model(), model()));
    for unavailable in [
        "unknown",
        "/tmp/missing.uqan",
        "https://example.invalid/model",
    ] {
        assert!(matches!(
            resources.load(&DictionaryRequest::Name(unavailable.into())),
            Err(DictionaryError::ResourceMissing(_))
        ));
    }
    let empty = NoriResources::with_resolver(
        Arc::new(|_: &DictionaryRequest| Ok(None)),
        ResourceLimits::default(),
    );
    assert!(matches!(
        empty.load_default(),
        Err(DictionaryError::ResourceMissing(_))
    ));
    assert_eq!(empty.cache_stats().dictionaries, 0);
}

#[test]
fn concurrent_user_compilation_shares_exact_rules_and_keeps_installed_tokenizers_immutable() {
    let resources = NoriResources::with_resolver(
        Arc::new(|_: &DictionaryRequest| Ok(None)),
        ResourceLimits {
            max_cached_user_dictionaries: 1,
            user_dictionary: UserDictionaryLimits {
                max_bytes: 64,
                ..UserDictionaryLimits::default()
            },
            ..ResourceLimits::default()
        },
    );
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let resources = resources.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                resources.compile_user("세종시 세종 시\n", model()).unwrap()
            })
        })
        .collect();
    let snapshots: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let first = &snapshots[0];
    assert!(snapshots
        .iter()
        .all(|snapshot| Arc::ptr_eq(first, snapshot)));
    assert_eq!(first.source(), "세종시 세종 시\n");
    assert_eq!(first.model_id(), model().id());
    let tokenizer = KoreanTokenizer::new(
        model().clone(),
        first.dictionary().cloned(),
        NoriOptions::default(),
    )
    .unwrap();
    assert!(resources.compile_user("가 가 나", model()).is_err());
    assert!(matches!(
        resources.compile_user(&"가".repeat(30), model()),
        Err(DictionaryError::Limit { .. })
    ));
    assert!(Arc::ptr_eq(
        first,
        &resources.compile_user(first.source(), model()).unwrap()
    ));
    let changed = resources
        .compile_user("세종시 세 종 시\n", model())
        .unwrap();
    assert_ne!(first.sha256(), changed.sha256());
    let replacement = KoreanTokenizer::new(
        model().clone(),
        changed.dictionary().cloned(),
        NoriOptions::default(),
    )
    .unwrap();
    let terms = |tokenizer: &KoreanTokenizer| {
        tokenizer
            .tokenize("세종시")
            .unwrap()
            .tokens
            .into_iter()
            .map(|token| String::from_utf16(&token.term_utf16).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(terms(&tokenizer), ["세종", "시"]);
    assert_eq!(terms(&replacement), ["세", "종", "시"]);
    assert_eq!(resources.cache_stats().user_dictionaries, 1);
    assert_eq!(resources.cache_stats().dictionaries, 0);
    assert_eq!(first.source(), "세종시 세종 시\n");
}
