//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
use uqa_analysis::nori::{
    DictionaryArtifact, DictionaryBytes, DictionaryRequest, NoriResources, ResourceLimits,
};

use super::*;

fn artifact() -> DictionaryArtifact {
    DictionaryArtifact {
        sha256: uqa_nori_data::BUNDLE_SHA256.parse().unwrap(),
        bytes: DictionaryBytes::Static(uqa_nori_data::BUNDLE),
    }
}

#[test]
fn name_resolution_is_reentrant_and_retained_handles_need_no_later_lookup() {
    let slot = Arc::new(Mutex::new(None::<AnalyzerResources>));
    let owner = Arc::downgrade(&slot);
    let available = Arc::new(AtomicBool::new(true));
    let flag = available.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let nori = NoriResources::with_resolver(
        Arc::new(move |request: &DictionaryRequest| {
            count.fetch_add(1, Ordering::SeqCst);
            let resources = owner.upgrade().unwrap().lock().unwrap().clone().unwrap();
            assert_eq!(
                resources
                    .compile(&Analyzer::default())
                    .unwrap()
                    .analyze("reentrant")
                    .unwrap(),
                ["reentrant"]
            );
            Ok((flag.load(Ordering::SeqCst)
                && matches!(request, DictionaryRequest::Name(name) if name=="custom"))
            .then(artifact))
        }),
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..Default::default()
        },
    );
    let resources = AnalyzerResources::with_nori_resources(AnalyzerLimits::default(), nori);
    *slot.lock().unwrap() = Some(resources.clone());
    let mut config = nori_analyzer();
    config.tokenizer = Tokenizer::Nori(NoriTokenizerConfig {
        dictionary: "custom".into(),
        user_dictionary: Some("세종시 세종 시".into()),
        ..Default::default()
    });
    config
        .token_filters
        .push(TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
            unicode_profile: "custom".into(),
        }));
    let compiled = resources.compile(&config).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    available.store(false, Ordering::SeqCst);
    assert!(Arc::ptr_eq(
        &compiled,
        &resources
            .restore_json(compiled.descriptor().canonical_json())
            .unwrap()
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(resources.compile(&config).is_err());
    assert_eq!(compiled.analyze("세종시").unwrap(), ["세종", "시"]);
    assert_eq!(compiled.normalize("İ UQA").unwrap(), "i uqa");
    assert_eq!(resources.cache_stats().analyzers, 2);
    *slot.lock().unwrap() = None;
}

#[test]
fn only_unicode_lowercase_needs_a_model_for_generic_korean_filters() {
    let nori = NoriResources::with_resolver(
        Arc::new(|_: &DictionaryRequest| Ok(None)),
        ResourceLimits::default(),
    );
    let resources = AnalyzerResources::with_nori_resources(AnalyzerLimits::default(), nori);
    let mut config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::NoriPartOfSpeech(NoriPOSConfig::default()),
            TokenFilter::NoriReadingForm(EmptyFilterConfig::default()),
            TokenFilter::NoriNumber(EmptyFilterConfig::default()),
        ],
        Vec::new(),
    );
    assert_eq!(
        resources
            .compile(&config)
            .unwrap()
            .analyze("３ 천")
            .unwrap(),
        ["3000"]
    );
    assert_eq!(resources.nori_resources().cache_stats().dictionaries, 0);
    config
        .token_filters
        .push(TokenFilter::UnicodeSimpleLowercase(
            SimpleLowercaseConfig::default(),
        ));
    assert!(matches!(
        resources.compile(&config),
        Err(AnalysisError::Dictionary(_))
    ));
    assert!(matches!(
        resources.compile(&nori_analyzer()),
        Err(AnalysisError::Dictionary(_))
    ));
    assert_eq!(resources.cache_stats().analyzers, 1);
}

#[test]
fn exact_user_source_identity_and_resource_limits_survive_descriptor_restoration() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut config = nori_analyzer();
    let mut fingerprints = Vec::new();
    for source in [
        None,
        Some(""),
        Some("# empty\n"),
        Some("세종시 세종 시\n"),
        Some("세종시 세 종 시\n"),
    ] {
        let Tokenizer::Nori(tokenizer) = &mut config.tokenizer else {
            unreachable!()
        };
        tokenizer.dictionary = exact_dictionary();
        tokenizer.user_dictionary = source.map(str::to_owned);
        let compiled = resources.compile(&config).unwrap();
        fingerprints.push(compiled.descriptor().fingerprint());
        let descriptor = AnalyzerDescriptor::from_json(
            compiled.descriptor().canonical_json(),
            AnalyzerLimits::default(),
        )
        .unwrap();
        assert!(Arc::ptr_eq(
            &compiled,
            &resources.restore(descriptor).unwrap()
        ));
    }
    fingerprints.sort();
    fingerprints.dedup();
    assert_eq!(fingerprints.len(), 5);
    let compiled = resources.compile(&config).unwrap();
    let strict = NoriResources::with_resolver(
        Arc::new(|_: &DictionaryRequest| Ok(Some(artifact()))),
        ResourceLimits {
            user_dictionary: UserDictionaryLimits {
                max_bytes: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let strict = AnalyzerResources::with_nori_resources(AnalyzerLimits::default(), strict);
    assert!(strict
        .restore_json(compiled.descriptor().canonical_json())
        .is_err());
    assert_eq!(strict.cache_stats().analyzers, 0);
    assert_eq!(compiled.analyze("세종시").unwrap(), ["세", "종", "시"]);
}

#[test]
fn restoration_requires_exact_resource_ids_and_revalidates_untrusted_user_rules() {
    use sha2::{Digest, Sha256};
    let compiled = nori_analyzer().compile().unwrap();
    let original: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
    let rehash = |value: &mut Value| {
        let mut bytes = Vec::new();
        super::super::nori_resources::canonical(&value["descriptor"], &mut bytes);
        let mut hash = Sha256::new();
        hash.update(b"UQA analyzer descriptor\0");
        hash.update(bytes);
        value["fingerprint"] = json!(format!("{:x}", hash.finalize()));
        value.to_string()
    };
    for (pointer, replacement) in [
        (
            "/descriptor/pipeline/tokenizer/dictionary",
            json!("lucene-10.5.1"),
        ),
        (
            "/descriptor/pipeline/token_filters/2/unicode_profile",
            json!("jdk21"),
        ),
        (
            "/descriptor/pipeline/token_filters/0/stop_tags",
            Value::Null,
        ),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(matches!(
            AnalyzerDescriptor::from_json(&rehash(&mut value), AnalyzerLimits::default()),
            Err(AnalysisError::Descriptor(_))
        ));
    }
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut value = original.clone();
    value["descriptor"]["pipeline"]["tokenizer"]["dictionary"] =
        json!(format!("sha256:{}", "0".repeat(64)));
    assert!(matches!(
        resources.restore_json(&rehash(&mut value)),
        Err(AnalysisError::Dictionary(_))
    ));
    let mut value = original;
    value["descriptor"]["pipeline"]["tokenizer"]["user_dictionary"] = json!("가 가 나");
    assert!(matches!(
        resources.restore_json(&rehash(&mut value)),
        Err(AnalysisError::Dictionary(_))
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
}

#[test]
fn concurrent_korean_compilation_interns_one_revision_and_survives_eviction() {
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_cached_analyzers: 1,
        ..Default::default()
    });
    let mut config = nori_analyzer();
    let Tokenizer::Nori(tokenizer) = &mut config.tokenizer else {
        unreachable!()
    };
    tokenizer.dictionary = exact_dictionary();
    tokenizer.user_dictionary = Some("세종시 세종 시".into());
    let user = UserDictionary::compile("세종시 세종 시", model(), UserDictionaryLimits::default())
        .unwrap();
    let native = KoreanAnalyzer::new(
        model().clone(),
        user,
        uqa_analysis::nori::NoriOptions::default(),
    )
    .unwrap();
    let expected: Arc<Vec<_>> = Arc::new(
        ["", "세종시", "나물은", "喜悲哀歡 İ UQA"]
            .into_iter()
            .map(|input| (input, native.analyze_tokens(input).unwrap()))
            .collect(),
    );
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let (resources, config, expected, barrier) = (
                resources.clone(),
                config.clone(),
                expected.clone(),
                barrier.clone(),
            );
            std::thread::spawn(move || {
                barrier.wait();
                let compiled = resources.compile(&config).unwrap();
                for _ in 0..8 {
                    for (input, expected) in expected.iter() {
                        assert_eq!(&compiled.analyze_tokens(input).unwrap(), expected);
                    }
                }
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
    resources.compile(&Analyzer::default()).unwrap();
    assert_eq!(handles[0].analyze("세종시").unwrap(), ["세종", "시"]);
    let restored = resources.restore(handles[0].descriptor().clone()).unwrap();
    assert!(!Arc::ptr_eq(&handles[0], &restored));
    assert_eq!(
        restored.analyze_tokens("세종시").unwrap(),
        handles[0].analyze_tokens("세종시").unwrap()
    );
}
