//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Analyzer, AnalyzerLimits, CharFilter, TokenFilter, Tokenizer};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("stored analyzer preparation must not read a transport")
    }
}

fn source() -> Arc<CompiledAnalyzer> {
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Synonym {
            synonyms: (0..256)
                .map(|n| {
                    (
                        format!("source{n}"),
                        vec![format!("result{n}{}", "尾".repeat(256))],
                    )
                })
                .collect(),
            synonyms_path: None,
        }],
        vec![CharFilter::HTMLStrip],
    );
    AnalyzerResources::new(AnalyzerLimits::default())
        .compile(&config)
        .unwrap()
}

fn descriptor() -> String {
    source().descriptor().canonical_json().to_owned()
}

fn input(
    json: &str,
    owner: AnalyzerResources,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Arc<CompiledAnalyzer>, AnalysisError> {
    let bytes = memory.reserve(json.len()).unwrap();
    let json = Budgeted::new(json.to_owned(), bytes);
    ImageDecoder::new(memory, token, |mut channel| async move {
        owner
            .restore_json_with_input(&json, &mut channel.input())
            .await
    })
    .unwrap()
}

fn complete(
    input: &mut ImageDecoder<Arc<CompiledAnalyzer>, AnalysisError>,
    steps: usize,
) -> AnalysisResult<Arc<CompiledAnalyzer>> {
    for _ in 0..100_000 {
        if let Some(value) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok(value);
        }
    }
    panic!("stored analyzer input did not finish")
}

#[test]
fn stored_analyzer_preparation_resumes_and_shares_one_completed_cache_identity() {
    let expected = source();
    let json = expected.descriptor().canonical_json();
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut first = input(json, owner.clone(), &memory, &token);
    let mut second = input(json, owner.clone(), &memory, &token);
    let mut calls = 0;
    let (first_value, second_value) = loop {
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        let left = first.advance(&mut NoRead, window).unwrap();
        let right = second.advance(&mut NoRead, window).unwrap();
        calls += 1;
        assert!(calls < 100_000);
        match (left, right) {
            (Some(left), Some(right)) => break (left, right),
            (None, None) => assert_eq!(owner.cache_stats().analyzers, 0),
            _ => panic!("equally scheduled preparations diverged"),
        }
    };
    assert!(calls > 128);
    assert!(Arc::ptr_eq(&first_value, &second_value));
    assert!(Arc::ptr_eq(
        &first_value,
        &owner.restore_json(json).unwrap()
    ));
    assert_eq!(
        first_value
            .analyze_tokens("<b>source0 source255</b>")
            .unwrap(),
        expected.analyze_tokens("<b>source0 source255</b>").unwrap()
    );
    assert_eq!(owner.cache_stats().analyzers, 1);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_analyzer_preparation_keeps_validation_before_cache_and_retires_terminal_input() {
    let json = descriptor();
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let cached = owner.restore_json(&json).unwrap();
    let mut corrupt: serde_json::Value = serde_json::from_str(&json).unwrap();
    corrupt["descriptor"]["algorithm_revision"] = 999.into();
    let text = serde_json::to_string(&corrupt).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let error = complete(&mut input(&text, owner.clone(), &memory, &token), 17).unwrap_err();
    assert_eq!(
        error.to_string(),
        owner.restore_json(&text).unwrap_err().to_string()
    );
    assert_eq!(memory.used(), 0);
    for stop in [1, 64, 1024] {
        for action in 0..4 {
            let token = CancellationToken::new();
            let mut pending = input(&json, owner.clone(), &memory, &token);
            for _ in 0..stop {
                assert!(pending
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            match action {
                0 => {
                    token.cancel();
                    assert!(pending
                        .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                        .is_err());
                }
                1 => assert!(pending
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 })
                    .is_err()),
                2 => (),
                _ => {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            let _pending = pending;
                            panic!("abandon retained preparation");
                        }));
                    assert!(result.is_err());
                    assert_eq!(memory.used(), 0);
                    continue;
                }
            }
            drop(pending);
            assert_eq!(memory.used(), 0);
        }
    }
    assert!(Arc::ptr_eq(&cached, &owner.restore_json(&json).unwrap()));
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
fn language(config: &Analyzer, text: &str, expected: &[&str]) {
    let original = AnalyzerResources::new(AnalyzerLimits::default())
        .compile(config)
        .unwrap();
    assert_eq!(original.analyze(text).unwrap(), expected);
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut retained = input(
        original.descriptor().canonical_json(),
        owner.clone(),
        &memory,
        &token,
    );
    let result = complete(&mut retained, 128).unwrap();
    assert_eq!(
        result.analyze_tokens(text).unwrap(),
        original.analyze_tokens(text).unwrap()
    );
    assert_eq!(
        result.descriptor().fingerprint(),
        original.descriptor().fingerprint()
    );
    assert!(Arc::ptr_eq(
        &result,
        &owner
            .restore_json(original.descriptor().canonical_json())
            .unwrap()
    ));
    assert_eq!(memory.used(), 0);
}

#[cfg(feature = "nori")]
#[test]
fn stored_korean_preparation_restores_exact_resources_and_native_user_rules() {
    language(
        &Analyzer::new(
            Tokenizer::Nori(crate::nori::NoriTokenizerConfig {
                user_dictionary: Some("세종시 세종 시".into()),
                ..Default::default()
            }),
            Vec::new(),
            Vec::new(),
        ),
        "세종시",
        &["세종", "시"],
    );
}

#[cfg(feature = "kuromoji")]
#[test]
fn stored_japanese_preparation_restores_exact_resources_and_native_user_rules() {
    language(
        &Analyzer::new(
            Tokenizer::Kuromoji(crate::kuromoji::KuromojiTokenizerConfig {
                user_dictionary: Some("東京大学,東京 大学,トウキョウ ダイガク,名詞".into()),
                ..Default::default()
            }),
            Vec::new(),
            Vec::new(),
        ),
        "東京大学",
        &["東京", "大学"],
    );
}
