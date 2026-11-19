//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{providers::destination, *};
use crate::tests::providers::Provider;
use uqa_analysis::{AnalyzerLimits, AnalyzerResources};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn archive_analysis_uses_the_supplied_owner_before_and_after_publication(
    #[case] provider: Provider,
) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repo"));
    let source = provider.open(&files.path().join("source"));
    let config = r#"{"tokenizer":{"type":"whitespace"}}"#;
    source.register_named_analyzer("archived", config).unwrap();
    let expected = source
        .analyze_text("archived", "original document")
        .unwrap();
    let root = capture(&source, &repository, None);
    drop(source);

    let authority = Engine::new();
    let control = StorageReadControl::with_limit(48 << 20);
    let options = RestoreOptions::new(32 << 20, 16 << 20);
    let small = AnalyzerResources::new(AnalyzerLimits {
        max_descriptor_bytes: 3,
        ..AnalyzerLimits::default()
    });
    let failed = files.path().join("too_small");
    let job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &failed),
            options.with_analyzer_resources(small),
            &control,
        )
        .unwrap();
    let id = job.id();
    assert_unusable(job, "limit");
    assert!(!failed.exists());
    if let Some(id) = id {
        assert!(authority
            .cancel_restore(
                &repository,
                id,
                destination(provider, &failed),
                options,
                &control
            )
            .unwrap()
            .is_none());
    }
    assert_eq!(control.memory().used(), 0);

    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let path = files.path().join("restored");
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            options.with_analyzer_resources(resources.clone()),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    assert_eq!(
        restored
            .analyze_text("archived", "original document")
            .unwrap(),
        expected
    );
    restored
        .register_named_analyzer("new_revision", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    assert!(resources.cache_stats().analyzers >= 1);
    if !matches!(provider, Provider::Memory) {
        restored.sql("BEGIN", &[]).unwrap();
        let sibling = restored.new_session().unwrap();
        assert_eq!(
            sibling
                .analyze_text("archived", "original document")
                .unwrap(),
            expected
        );
        restored.sql("ROLLBACK", &[]).unwrap();
    }
}

#[cfg(feature = "nori")]
#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_dictionary_restore_requires_the_archived_digest(#[case] provider: Provider) {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_analysis::nori::{
        DictionaryArtifact, DictionaryBytes, DictionaryRequest, NoriResources, ResourceLimits,
    };

    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repo"));
    let source = provider.open(&files.path().join("source"));
    let config = serde_json::to_string(&uqa_analysis::nori::nori_analyzer()).unwrap();
    source
        .register_named_analyzer("korean_saved", &config)
        .unwrap();
    let expected = source.analyze_text("korean_saved", "한국어").unwrap();
    let dictionary = NoriResources::default().load_default().unwrap();
    let hash = dictionary.sha256();
    let content: Arc<[u8]> = Arc::from(dictionary.bytes());
    let root = capture(&source, &repository, None);
    drop(source);

    let control = StorageReadControl::with_limit(128 << 20);
    let limits = RestoreOptions::new(64 << 20, 32 << 20);
    let authority = Engine::new();
    for (directory, content, expected_error) in [
        ("missing", None, Some("dictionary")),
        (
            "invalid",
            Some(Arc::from(&b"incorrect dictionary"[..])),
            Some("hash mismatch"),
        ),
        ("available", Some(content), None),
    ] {
        let lookups = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&lookups);
        let resources = AnalyzerResources::builder(AnalyzerLimits::default())
            .nori_resources(NoriResources::with_resolver(
                Arc::new(move |request: &DictionaryRequest| {
                    assert_eq!(request, &DictionaryRequest::Sha256(hash));
                    seen.fetch_add(1, Ordering::SeqCst);
                    Ok(content.as_ref().map(|bytes| DictionaryArtifact {
                        sha256: hash,
                        bytes: DictionaryBytes::Shared(Arc::clone(bytes)),
                    }))
                }),
                ResourceLimits::default(),
            ))
            .build();
        let path = files.path().join(directory);
        let mut job = authority
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                limits.with_analyzer_resources(resources.clone()),
                &control,
            )
            .unwrap();
        if let Some(expected) = expected_error {
            let id = job.id().unwrap();
            assert_unusable(job, expected);
            assert!(!path.exists());
            assert!(authority
                .cancel_restore(
                    &repository,
                    id,
                    destination(provider, &path),
                    limits,
                    &control
                )
                .unwrap()
                .is_none());
        } else {
            prepare(&mut job);
            let restored = job.publish().unwrap();
            assert_eq!(
                restored.analyze_text("korean_saved", "한국어").unwrap(),
                expected
            );
            let supplied = restored.storage.provider.as_ref().unwrap().clone();
            drop((restored, job));
            let reopened =
                Engine::from_persistent_provider_with_analyzer_resources(supplied, resources)
                    .unwrap();
            assert_eq!(
                reopened.analyze_text("korean_saved", "한국어").unwrap(),
                expected
            );
        }
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
    }
}
