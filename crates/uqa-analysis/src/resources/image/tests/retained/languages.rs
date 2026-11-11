//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn isolated() -> AnalyzerResources {
    let owner = AnalyzerResources::builder(crate::AnalyzerLimits::default());
    #[cfg(feature = "nori")]
    let owner = owner.nori_resources(crate::nori::NoriResources::with_resolver(
        Arc::new(|_: &crate::nori::DictionaryRequest| panic!("image used a host Korean resolver")),
        crate::nori::ResourceLimits::default(),
    ));
    #[cfg(feature = "kuromoji")]
    let owner = owner.kuromoji_resources(crate::kuromoji::KuromojiResources::with_resolver(
        Arc::new(|_: &crate::kuromoji::DictionaryRequest| {
            panic!("image used a host Japanese resolver")
        }),
        crate::kuromoji::ResourceLimits::default(),
    ));
    owner.build()
}

fn images(source: &CompiledAnalyzer, memory: &MemoryBudget) -> Vec<AnalyzerResourceInput> {
    source
        .dictionary_resources()
        .map(|resource| {
            let reservation = memory.reserve(resource.bytes().len()).unwrap();
            AnalyzerResourceInput {
                kind: resource.kind(),
                sha256: resource.sha256(),
                bytes: Budgeted::new(resource.bytes().to_vec(), reservation)
                    .into_shared()
                    .unwrap(),
            }
        })
        .collect()
}

fn verify(config: &crate::Analyzer, text: &str, expected: &[&str]) {
    let source = AnalyzerResources::new(crate::AnalyzerLimits::default())
        .compile(config)
        .unwrap();
    assert_eq!(source.analyze(text).unwrap(), expected);
    let expected_tokens = source.analyze_tokens(text).unwrap();
    let descriptor = source.descriptor().clone();
    let encoded = MemoryBudget::new(64 << 20);
    let inputs = images(&source, &encoded);
    let pointers: Vec<_> = inputs
        .iter()
        .map(|input| (input.kind, input.sha256, input.bytes.as_ptr()))
        .collect();
    drop(source);
    let owner = isolated();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = preparing(owner.clone(), descriptor.clone(), inputs, &memory, &token);
    let mut calls = 0;
    let restored = loop {
        calls += 1;
        if let Some(restored) = input
            .advance(
                &mut NoRead,
                ImageDecodeWindow {
                    bytes: 1,
                    steps: 128,
                },
            )
            .unwrap()
        {
            break restored;
        }
        assert!(calls < 100_000);
    };
    assert!(calls > 100);
    assert_eq!(restored.analyze_tokens(text).unwrap(), expected_tokens);
    assert!(Arc::ptr_eq(restored.descriptor(), &descriptor));
    assert_eq!(
        restored
            .dictionary_resources()
            .map(|resource| (
                resource.kind(),
                resource.sha256(),
                resource.bytes().as_ptr()
            ))
            .collect::<Vec<_>>(),
        pointers
    );
    assert_eq!(owner.cache_stats().analyzers, 0);
    #[cfg(feature = "nori")]
    assert_eq!(owner.nori_resources().cache_stats().dictionaries, 0);
    #[cfg(feature = "kuromoji")]
    assert_eq!(owner.kuromoji_resources().cache_stats().dictionaries, 0);
    assert!(encoded.used() > 0);
    drop((input, restored));
    assert_eq!(memory.used(), 0);
    assert_eq!(encoded.used(), 0);
    assert_eq!(Arc::strong_count(&descriptor), 1);
}

#[cfg(feature = "nori")]
#[test]
fn retained_korean_image_preserves_user_rules_dictionary_bytes_and_private_ownership() {
    let filters = Vec::new();
    #[cfg(feature = "kuromoji")]
    let filters = {
        let mut filters = filters;
        filters.push(crate::TokenFilter::KuromojiStop(
            crate::kuromoji::KuromojiStopConfig {
                words: Some(vec!["unused".into()]),
                ignore_case: true,
                dictionary: None,
            },
        ));
        filters
    };
    let config = crate::Analyzer::new(
        crate::Tokenizer::Nori(crate::nori::NoriTokenizerConfig {
            user_dictionary: Some("세종시 세종 시\n세종시 세 종 시".into()),
            ..Default::default()
        }),
        filters,
        Vec::new(),
    );
    verify(&config, "세종시", &["세종", "시"]);
}

#[cfg(feature = "kuromoji")]
#[test]
fn retained_japanese_image_prepares_user_rules_and_large_case_insensitive_stop_sets() {
    let words = (0..256)
        .map(|i| format!("PREFIX{}{:04}", "尾".repeat(512), i))
        .chain(["VOID".into(), "void".into(), "大学".into()])
        .collect();
    let config = crate::Analyzer::new(
        crate::Tokenizer::Kuromoji(crate::kuromoji::KuromojiTokenizerConfig {
            user_dictionary: Some("東京大学,東京 大学,トウキョウ ダイガク,名詞".into()),
            ..Default::default()
        }),
        vec![crate::TokenFilter::KuromojiStop(
            crate::kuromoji::KuromojiStopConfig {
                words: Some(words),
                ignore_case: true,
                dictionary: None,
            },
        )],
        Vec::new(),
    );
    verify(&config, "東京大学", &["東京"]);
}

#[cfg(feature = "nori")]
#[test]
fn retained_image_failure_order_keeps_unused_artifacts_lazy_and_retires_requested_bytes() {
    let owner = isolated();
    let source = AnalyzerResources::new(crate::AnalyzerLimits::default())
        .compile(&crate::Analyzer::default())
        .unwrap();
    let extra = AnalyzerResourceInput {
        kind: AnalyzerResourceKind::Nori,
        sha256: [9; 32],
        bytes: empty_bytes(),
    };
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = preparing(
        owner.clone(),
        source.descriptor().clone(),
        vec![extra],
        &memory,
        &token,
    );
    loop {
        match input.advance(
            &mut NoRead,
            ImageDecodeWindow {
                bytes: 1,
                steps: 17,
            },
        ) {
            Ok(None) => (),
            Err(AnalysisError::Descriptor(
                "image resources differ from the descriptor closure",
            )) => break,
            other => panic!(
                "unused artifact changed diagnostic order: {}",
                other
                    .err()
                    .map_or_else(|| "completed".into(), |error| error.to_string())
            ),
        }
    }
    drop(input);
    assert_eq!(memory.used(), 0);
    let config = crate::Analyzer::new(
        crate::Tokenizer::Nori(crate::nori::NoriTokenizerConfig::default()),
        Vec::new(),
        Vec::new(),
    );
    let source = AnalyzerResources::new(crate::AnalyzerLimits::default())
        .compile(&config)
        .unwrap();
    assert!(owner
        .restore_image(source.descriptor().clone(), Vec::new())
        .unwrap_err()
        .to_string()
        .contains("unavailable"));
    for stop in [64, 4096] {
        let encoded = MemoryBudget::new(64 << 20);
        let inputs = images(&source, &encoded);
        let token = CancellationToken::new();
        let mut input = preparing(
            owner.clone(),
            source.descriptor().clone(),
            inputs,
            &memory,
            &token,
        );
        for _ in 0..stop {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        token.cancel();
        assert!(input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        drop(input);
        assert_eq!(encoded.used(), 0);
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(owner.nori_resources().cache_stats().dictionaries, 0);
}
