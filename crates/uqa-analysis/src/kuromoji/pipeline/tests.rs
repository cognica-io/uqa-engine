//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::kuromoji::{DictionaryArtifact, DictionaryBytes, ResourceHash, ResourceLimits};
use crate::{AnalyzerLimits, AnalyzerResources, NormalizationConfig, Tokenizer};
use parking_lot::Mutex;

mod corpus;
mod tokenizer;

#[test]
fn normalization_profiles_snapshot_alias_changes_and_restore_only_exact_artifacts() {
    let current = Arc::new(Mutex::new(Some(DictionaryBytes::Static(
        uqa_kuromoji_data::BUNDLE,
    ))));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kuromoji = KuromojiResources::with_resolver(
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
    let owner = || {
        AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(kuromoji.clone())
            .build()
    };
    let resources = owner();
    let config = Analyzer::new(
        Tokenizer::Kuromoji(KuromojiTokenizerConfig {
            dictionary: "current".into(),
            ..Default::default()
        }),
        vec![
            crate::TokenFilter::Stop {
                language: String::new(),
                custom_words: Vec::new(),
            },
            crate::TokenFilter::UnicodeSimpleLowercase(crate::SimpleLowercaseConfig {
                unicode_profile: UnicodeProfile::Kuromoji {
                    dictionary: "current".into(),
                }
                .into(),
            }),
        ],
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::UnicodeSimpleLowercase {
        profile: UnicodeProfile::Kuromoji {
            dictionary: "current".into(),
        },
    });
    let retained = resources.compile(&config).unwrap();
    assert_eq!(
        requests.lock().as_slice(),
        [DictionaryRequest::Name("current".into())]
    );
    assert_eq!(retained.normalize("UQA").unwrap(), "uqa");
    assert_eq!(retained.analyze("UQA").unwrap(), ["uqa"]);
    let reopened = owner()
        .restore_json(retained.descriptor().canonical_json())
        .unwrap();
    assert_eq!(
        requests.lock()[1],
        DictionaryRequest::Sha256(uqa_kuromoji_data::BUNDLE_SHA256.parse().unwrap())
    );
    assert_eq!(reopened.normalize("UQA").unwrap(), "uqa");
    let replacement = crate::kuromoji::tests::fixtures::bundle();
    *current.lock() = Some(DictionaryBytes::Shared(replacement.into()));
    let changed = resources.compile(&config).unwrap();
    assert_ne!(
        changed.descriptor().fingerprint(),
        retained.descriptor().fingerprint()
    );
    assert_eq!(changed.normalize("UQA").unwrap(), "UQA");
    assert_eq!(changed.analyze("UQA").unwrap(), ["UQA"]);
    assert_eq!(retained.normalize("UQA").unwrap(), "uqa");
    assert_eq!(retained.analyze("UQA").unwrap(), ["uqa"]);
    let fresh = owner();
    assert!(fresh
        .restore_json(retained.descriptor().canonical_json())
        .is_err());
    assert_eq!(fresh.cache_stats().analyzers, 0);
    *current.lock() = None;
    assert!(resources.compile(&config).is_err());
    assert!(owner()
        .restore_json(changed.descriptor().canonical_json())
        .is_err());
    assert!(Arc::ptr_eq(
        &retained,
        &resources
            .restore_json(retained.descriptor().canonical_json())
            .unwrap()
    ));
    assert_eq!(retained.normalize("UQA").unwrap(), "uqa");
    assert_eq!(changed.normalize("UQA").unwrap(), "UQA");
    assert_eq!(resources.cache_stats().analyzers, 2);
}
