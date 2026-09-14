//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use parking_lot::Mutex;

use super::*;
use crate::kuromoji::{DictionaryError, DictionaryLimits, UserDictionaryLimits};

fn artifact(bytes: Arc<[u8]>) -> DictionaryArtifact {
    DictionaryArtifact {
        sha256: ResourceHash::of(&bytes),
        bytes: DictionaryBytes::Shared(bytes),
    }
}

fn fixture(cost: i16) -> Arc<[u8]> {
    let mut sections = crate::kuromoji::tests::fixtures::sections();
    sections[3].bytes[8..10].copy_from_slice(&cost.to_le_bytes());
    crate::kuromoji::frame::encode(&sections, DictionaryLimits::default())
        .unwrap()
        .into()
}

fn supplied(bytes: Arc<[u8]>, limits: ResourceLimits) -> KuromojiResources {
    KuromojiResources::with_resolver(
        Arc::new(move |_: &DictionaryRequest| Ok(Some(artifact(bytes.clone())))),
        limits,
    )
}

#[test]
fn japanese_resources_retain_exact_bundle_and_compiled_user_source() {
    let resources = KuromojiResources::default();
    let dictionary = resources.load_default().unwrap();
    assert_eq!(
        dictionary.sha256().to_string(),
        uqa_kuromoji_data::BUNDLE_SHA256
    );
    assert_eq!(
        dictionary.model().id().to_string(),
        uqa_kuromoji_data::DICTIONARY_ID
    );
    let exact = DictionaryRequest::Sha256(dictionary.sha256());
    assert!(Arc::ptr_eq(&dictionary, &resources.load(&exact).unwrap()));
    let source = "東京大学,東京 大学,トウキョウ ダイガク,名詞";
    let rules = resources.compile_user(source, dictionary.model()).unwrap();
    assert_eq!(rules.sha256(), ResourceHash::of(source.as_bytes()));
    assert_eq!(rules.model_id(), dictionary.model().id());
    assert_eq!(rules.source(), source);
    assert!(Arc::ptr_eq(
        &rules,
        &resources.compile_user(source, dictionary.model()).unwrap()
    ));
    let user = rules.dictionary().unwrap();
    let entry = user.entry(user.lookup("東京大学").unwrap()).unwrap();
    assert_eq!(entry.segment_lengths(), [2, 2]);
    assert_eq!(
        user.word(entry.word_base()).unwrap().reading().unwrap(),
        "トウキョウ"
    );
    let empty = resources
        .compile_user("# retained\n", dictionary.model())
        .unwrap();
    assert!(empty.dictionary().is_none());
    assert_eq!(empty.source(), "# retained\n");
    assert_ne!(empty.sha256(), rules.sha256());
    assert!(resources
        .compile_user(
            "東京,東京,トウキョウ,名詞\n東京,東京,トウキョウ,名詞",
            dictionary.model()
        )
        .is_err());
    assert!(Arc::ptr_eq(
        &rules,
        &resources.compile_user(source, dictionary.model()).unwrap()
    ));
}

#[test]
fn japanese_resolver_failures_preserve_retained_content_and_recover() {
    let bytes = fixture(-10);
    let current = Arc::new(Mutex::new(artifact(bytes.clone())));
    let resources = KuromojiResources::with_resolver(
        Arc::new({
            let current = current.clone();
            move |request: &DictionaryRequest| {
                if matches!(request, DictionaryRequest::Name(name) if name == "missing") {
                    return Ok(None);
                }
                let current = current.lock();
                Ok(Some(DictionaryArtifact {
                    sha256: current.sha256,
                    bytes: current.bytes.clone(),
                }))
            }
        }),
        ResourceLimits::default(),
    );
    let named = DictionaryRequest::Name("alias".into());
    let retained = resources.load(&named).unwrap();
    let wrong = ResourceHash::of(b"wrong");
    assert!(matches!(resources.load(&DictionaryRequest::Sha256(wrong)),
        Err(DictionaryError::ResourceHashMismatch { expected, .. }) if expected == wrong));
    current.lock().sha256 = wrong;
    assert!(matches!(
        resources.load(&named),
        Err(DictionaryError::ResourceHashMismatch { .. })
    ));
    assert!(matches!(
        resources.load(&DictionaryRequest::Name("missing".into())),
        Err(DictionaryError::ResourceMissing(_))
    ));
    *current.lock() = artifact(Arc::from(uqa_nori_data::BUNDLE));
    assert!(matches!(
        resources.load(&named),
        Err(DictionaryError::Invalid { .. })
    ));
    assert_eq!(resources.cache_stats().dictionaries, 1);
    assert_eq!(retained.model().connection_cost(0, 0), Some(-10));
    *current.lock() = artifact(bytes);
    assert!(Arc::ptr_eq(&retained, &resources.load(&named).unwrap()));
    *current.lock() = artifact(fixture(12));
    let replacement = resources.load(&named).unwrap();
    assert_ne!(replacement.model().id(), retained.model().id());
    assert_eq!(replacement.model().connection_cost(0, 0), Some(12));
}

#[test]
fn japanese_resource_limits_are_independent_of_other_owners_and_live_handles() {
    let bytes = fixture(0);
    let owner = supplied(bytes.clone(), ResourceLimits::default());
    let model = owner.load_default().unwrap();
    let stricter = supplied(
        bytes.clone(),
        ResourceLimits {
            dictionary: DictionaryLimits {
                max_encoded_bytes: bytes.len() - 1,
                ..DictionaryLimits::default()
            },
            ..ResourceLimits::default()
        },
    );
    assert!(matches!(
        stricter.load(&DictionaryRequest::Sha256(model.sha256())),
        Err(DictionaryError::Limit { .. })
    ));
    let uncached = supplied(
        bytes,
        ResourceLimits {
            max_cached_dictionaries: 0,
            max_cached_user_dictionaries: 1,
            user_dictionary: UserDictionaryLimits {
                max_bytes: 8,
                ..UserDictionaryLimits::default()
            },
            ..ResourceLimits::default()
        },
    );
    let first = uncached.load_default().unwrap();
    let second = uncached.load_default().unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(uncached.cache_stats().dictionaries, 0);
    let empty = uncached.compile_user("# first", first.model()).unwrap();
    uncached.compile_user("# second", first.model()).unwrap();
    assert_eq!(empty.source(), "# first");
    assert_eq!(uncached.cache_stats().user_dictionaries, 1);
    assert!(matches!(
        uncached.compile_user("# excessive", first.model()),
        Err(DictionaryError::Limit { .. })
    ));
    assert_eq!(first.model().id(), model.model().id());
}

#[test]
fn analyzer_builder_installs_independent_language_resource_owners() {
    let japanese = supplied(fixture(0), ResourceLimits::default());
    let held = japanese.load_default().unwrap();
    let builder = crate::AnalyzerResources::builder(crate::AnalyzerLimits::default())
        .kuromoji_resources(japanese);
    #[cfg(feature = "nori")]
    let korean = crate::nori::NoriResources::default();
    #[cfg(feature = "nori")]
    let korean_held = korean.load_default().unwrap();
    #[cfg(feature = "nori")]
    let builder = builder.nori_resources(korean);
    let resources = builder.build();
    assert!(Arc::ptr_eq(
        &held,
        &resources.kuromoji_resources().load_default().unwrap()
    ));
    #[cfg(feature = "nori")]
    assert!(Arc::ptr_eq(
        &korean_held,
        &resources.nori_resources().load_default().unwrap()
    ));
    let compiled = resources
        .compile(&crate::standard_analyzer("english"))
        .unwrap();
    assert_eq!(compiled.analyze("The cats").unwrap(), ["cat"]);
    let restored = resources
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    assert!(Arc::ptr_eq(&compiled, &restored));
}
