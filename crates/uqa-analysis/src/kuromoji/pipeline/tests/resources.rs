//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::kuromoji::{CompletionMode, JapaneseFilter, KuromojiPOSConfig, KuromojiStopConfig};
use serde_json::json;

#[test]
fn builtin_resources_resolve_one_alias_per_pipeline_with_reentrant_callbacks() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let slot = Arc::new(Mutex::new(None::<AnalyzerResources>));
    let weak = Arc::downgrade(&slot);
    let available = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let resources = KuromojiResources::with_resolver(
        Arc::new({
            let available = available.clone();
            let calls = calls.clone();
            move |_: &DictionaryRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                let owner = weak.upgrade().unwrap().lock().clone().unwrap();
                assert_eq!(
                    owner
                        .compile(&Analyzer::default())
                        .unwrap()
                        .analyze("reentrant")
                        .unwrap(),
                    ["reentrant"]
                );
                Ok(available
                    .load(Ordering::SeqCst)
                    .then_some(DictionaryArtifact {
                        bytes: DictionaryBytes::Static(uqa_kuromoji_data::BUNDLE),
                        sha256: uqa_kuromoji_data::BUNDLE_SHA256.parse().unwrap(),
                    }))
            }
        }),
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..Default::default()
        },
    );
    let owner = AnalyzerResources::builder(AnalyzerLimits::default())
        .kuromoji_resources(resources.clone())
        .build();
    *slot.lock() = Some(owner.clone());
    let mut retained = Vec::new();
    for mut config in [
        crate::kuromoji::kuromoji_analyzer(),
        crate::kuromoji::kuromoji_completion_analyzer(),
    ] {
        let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
            unreachable!()
        };
        tokenizer.dictionary = "current".into();
        for filter in &mut config.token_filters {
            match filter {
                TokenFilter::KuromojiPartOfSpeech(stage) => {
                    stage.dictionary = Some("current".into());
                }
                TokenFilter::KuromojiStop(stage) => stage.dictionary = Some("current".into()),
                TokenFilter::KuromojiCompletion(stage) => stage.dictionary = "current".into(),
                TokenFilter::UnicodeSimpleLowercase(stage) => {
                    *stage.unicode_profile.kuromoji_dictionary_mut().unwrap() = "current".into();
                }
                _ => {}
            }
        }
        if let Some(UnicodeProfile::Kuromoji { dictionary }) = config
            .normalization
            .as_mut()
            .and_then(crate::NormalizationConfig::profile_mut)
        {
            *dictionary = "current".into();
        }
        let before = calls.load(Ordering::SeqCst);
        let compiled = owner.compile(&config).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), before + 1);
        assert_eq!(compiled.analyze("ＵＱＡ").unwrap(), ["uqa"]);
        retained.push((config, compiled));
    }
    available.store(false, Ordering::SeqCst);
    for (config, compiled) in retained {
        let before = calls.load(Ordering::SeqCst);
        assert!(Arc::ptr_eq(
            &compiled,
            &owner
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap()
        ));
        assert_eq!(calls.load(Ordering::SeqCst), before);
        assert!(owner.compile(&config).is_err());
        let fresh = AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(resources.clone())
            .build();
        assert!(fresh
            .restore_json(compiled.descriptor().canonical_json())
            .is_err());
        assert_eq!(fresh.cache_stats().analyzers, 0);
        assert_eq!(compiled.analyze("ＵＱＡ").unwrap(), ["uqa"]);
    }
    *slot.lock() = None;
}

struct MutableResources {
    resources: KuromojiResources,
    current: Arc<Mutex<Option<DictionaryBytes>>>,
    requests: Arc<Mutex<Vec<DictionaryRequest>>>,
}

fn mutable_resources(bytes: DictionaryBytes) -> MutableResources {
    let current = Arc::new(Mutex::new(Some(bytes)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let resources = KuromojiResources::with_resolver(
        Arc::new({
            let current = current.clone();
            let requests = requests.clone();
            move |request: &DictionaryRequest| {
                requests.lock().push(request.clone());
                Ok(current.lock().clone().map(|bytes| DictionaryArtifact {
                    sha256: ResourceHash::of(bytes.as_ref()),
                    bytes,
                }))
            }
        }),
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..Default::default()
        },
    );
    MutableResources {
        resources,
        current,
        requests,
    }
}

#[test]
fn default_stop_sets_restore_without_their_original_dictionary_and_follow_alias_changes() {
    let MutableResources {
        resources: kuromoji,
        current,
        requests,
    } = mutable_resources(DictionaryBytes::Static(uqa_kuromoji_data::BUNDLE));
    let owner = || {
        AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(kuromoji.clone())
            .build()
    };
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::KuromojiPartOfSpeech(KuromojiPOSConfig {
                stop_tags: None,
                dictionary: Some("current".into()),
            }),
            TokenFilter::KuromojiStop(KuromojiStopConfig {
                words: None,
                ignore_case: false,
                dictionary: Some("current".into()),
            }),
        ],
        Vec::new(),
    );
    let retained = owner().compile(&config).unwrap();
    assert_eq!(
        requests.lock().as_slice(),
        [DictionaryRequest::Name("current".into())]
    );
    assert_eq!(retained.analyze("は に UQA").unwrap(), ["UQA"]);
    *current.lock() = Some(DictionaryBytes::Shared(
        crate::kuromoji::tests::fixtures::bundle().into(),
    ));
    let changed = owner().compile(&config).unwrap();
    assert_ne!(
        retained.descriptor().fingerprint(),
        changed.descriptor().fingerprint()
    );
    assert_eq!(changed.analyze("は に UQA").unwrap(), ["に", "UQA"]);
    *current.lock() = None;
    for compiled in [retained, changed] {
        assert!(!compiled.descriptor().canonical_json().contains("sha256:"));
        let restored = owner()
            .restore_json(compiled.descriptor().canonical_json())
            .unwrap();
        assert_eq!(
            restored.analyze_tokens("は に UQA").unwrap(),
            compiled.analyze_tokens("は に UQA").unwrap()
        );
    }
    assert_eq!(requests.lock().len(), 2);
    assert_eq!(kuromoji.cache_stats().dictionaries, 0);
}

#[test]
fn prepared_filters_drop_models_used_only_to_expand_default_sets() {
    let resources = mutable_resources(DictionaryBytes::Static(uqa_kuromoji_data::BUNDLE)).resources;
    let profile = resources
        .load(&DictionaryRequest::Name("profile".into()))
        .unwrap();
    for (stage, retained) in [
        (JapaneseFilter::PartOfSpeech { stop_tags: None }, false),
        (
            JapaneseFilter::Stop {
                words: None,
                ignore_case: false,
            },
            false,
        ),
        (
            JapaneseFilter::Stop {
                words: None,
                ignore_case: true,
            },
            true,
        ),
        (JapaneseFilter::SimpleLowercase, true),
        (
            JapaneseFilter::Completion {
                mode: CompletionMode::Index,
            },
            true,
        ),
    ] {
        let before = Arc::strong_count(&profile);
        let prepared = PreparedKuromojiFilter::new(&stage, Some(profile.clone())).unwrap();
        assert_eq!(
            Arc::strong_count(&profile),
            before + usize::from(retained),
            "{stage:?}"
        );
        drop(prepared);
        assert_eq!(Arc::strong_count(&profile), before, "{stage:?}");
    }
    let weak = Arc::downgrade(&profile);
    drop(profile);
    assert!(weak.upgrade().is_none());
}

#[test]
fn expanded_default_sets_hit_descriptor_bounds_before_resolving_later_stages() {
    let MutableResources {
        resources: kuromoji,
        requests,
        ..
    } = mutable_resources(DictionaryBytes::Static(uqa_kuromoji_data::BUNDLE));
    let config: Analyzer = serde_json::from_value(json!({"token_filters":[
        {"type":"kuromoji_part_of_speech", "dictionary":"first"},
        {"type":"kuromoji_completion", "dictionary":"later"}
    ]}))
    .unwrap();
    let limits = AnalyzerLimits {
        max_descriptor_bytes: serde_json::to_vec(&config).unwrap().len() + 32,
        ..Default::default()
    };
    let owner = AnalyzerResources::builder(limits)
        .kuromoji_resources(kuromoji)
        .build();
    assert!(matches!(
        owner.compile(&config),
        Err(AnalysisError::ResourceLimit {
            resource: "analyzer descriptor bytes",
            ..
        })
    ));
    assert_eq!(
        requests.lock().as_slice(),
        [DictionaryRequest::Name("first".into())]
    );
    assert_eq!(owner.cache_stats().analyzers, 0);
}

#[test]
fn stop_snapshots_apply_custom_unicode_mappings_once_on_every_restore() {
    use crate::morphology::{
        io::Writer,
        unicode::{Properties, UnicodeRange, UnicodeTable, CODE_POINTS},
    };
    let table = UnicodeTable {
        ranges: [
            (65, 0, 0),
            (67, 0, 1),
            (0xd800, 0, 0),
            (0xe000, 19, 0),
            (CODE_POINTS, 0, 0),
        ]
        .map(|(end, category, lowercase_delta)| UnicodeRange {
            end,
            properties: Properties {
                category,
                flags: 0,
                script: 2,
                lowercase_delta,
            },
        })
        .into(),
    };
    let mut bytes = Writer::default();
    table.encode(&mut bytes).unwrap();
    let mut sections = crate::kuromoji::tests::fixtures::sections();
    sections
        .iter_mut()
        .find(|section| section.kind == 6)
        .unwrap()
        .bytes = bytes.0;
    let bytes =
        crate::kuromoji::frame::encode(&sections, crate::kuromoji::DictionaryLimits::default())
            .unwrap();
    let kuromoji = mutable_resources(DictionaryBytes::Shared(bytes.into())).resources;
    let owner = || {
        AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(kuromoji.clone())
            .build()
    };
    let config: Analyzer = serde_json::from_value(json!({"token_filters":[{"type":"kuromoji_stop", "dictionary":"custom", "words":["A"], "ignore_case":true}]})).unwrap();
    let compiled = owner().compile(&config).unwrap();
    assert_eq!(compiled.analyze("A B").unwrap(), ["B"]);
    let restored = owner()
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    assert_eq!(restored.analyze("A B").unwrap(), ["B"]);
    let config = serde_json::to_value(restored.descriptor().configuration().unwrap()).unwrap();
    assert_eq!(config["token_filters"][0]["words"], json!(["A"]));
}
