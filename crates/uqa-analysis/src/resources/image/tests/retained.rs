//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("resolved image preparation cannot reread its transport");
    }
}

fn preparing(
    owner: AnalyzerResources,
    descriptor: Arc<AnalyzerDescriptor>,
    inputs: Vec<AnalyzerResourceInput>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Arc<CompiledAnalyzer>, AnalysisError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        owner
            .restore_image_with_input(descriptor, inputs, &mut channel.input())
            .await
    })
    .unwrap()
}

fn descriptor() -> Arc<AnalyzerDescriptor> {
    let mut mappings: std::collections::BTreeMap<_, _> = (0..128)
        .map(|i| (format!("m{i:04}{}", "尾".repeat(256)), "target".repeat(512)))
        .collect();
    mappings.insert("source".into(), "alpha".into());
    let mut synonyms: std::collections::BTreeMap<_, _> = (0..128)
        .map(|i| {
            (
                format!("s{i:04}{}", "한🙂".repeat(128)),
                vec!["value".repeat(819)],
            )
        })
        .collect();
    synonyms.insert("alpha".into(), vec!["first".into(), "second".into()]);
    let config = crate::Analyzer::new(
        crate::Tokenizer::Keyword,
        vec![
            crate::TokenFilter::Synonym {
                synonyms,
                synonyms_path: None,
            },
            crate::TokenFilter::Stop {
                language: String::new(),
                custom_words: vec!["second".into()],
            },
        ],
        vec![crate::CharFilter::Mapping { mapping: mappings }],
    );
    AnalyzerResources::new(crate::AnalyzerLimits::default())
        .compile(&config)
        .unwrap()
        .descriptor()
        .clone()
}

#[test]
fn retained_analyzer_image_preparation_preserves_exact_descriptor_and_independent_token_output() {
    let descriptor = descriptor();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let owner = AnalyzerResources::new(crate::AnalyzerLimits::default());
    let mut input = preparing(
        owner.clone(),
        descriptor.clone(),
        Vec::new(),
        &memory,
        &token,
    );
    let mut calls = 0;
    let result = loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap();
        });
        assert!(
            allocation.bytes_total < 64 * 1024,
            "image preparation poll {calls}: {} bytes",
            allocation.bytes_total
        );
        calls += 1;
        if let Some(result) = result {
            break result;
        }
        assert!(calls < 100_000);
    };
    assert!(calls > 1024);
    assert_eq!(result.analyze("source").unwrap(), ["alpha", "first"]);
    assert!(Arc::ptr_eq(result.descriptor(), &descriptor));
    assert_eq!(owner.cache_stats().analyzers, 0);
    drop((input, result));
    assert_eq!(memory.used(), 0);
    assert_eq!(Arc::strong_count(&descriptor), 1);
}

#[test]
fn retained_analyzer_image_preparation_retires_candidates_on_cancel_invalid_windows_and_drop() {
    let descriptor = descriptor();
    for stop in [1, 64, 1024, 4096] {
        for action in 0..3 {
            let owner = AnalyzerResources::new(crate::AnalyzerLimits::default());
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = preparing(
                owner.clone(),
                descriptor.clone(),
                Vec::new(),
                &memory,
                &token,
            );
            for _ in 0..stop {
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            if action == 0 {
                token.cancel();
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            } else if action == 1 {
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 0, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(memory.used(), 0);
            assert_eq!(Arc::strong_count(&descriptor), 1);
            assert_eq!(owner.cache_stats().analyzers, 0);
        }
    }
}

#[test]
fn retained_descriptor_and_image_compose_without_transport_replay_or_input_owner_leaks() {
    let expected = descriptor();
    let encoded = MemoryBudget::new(4 << 20);
    let json = expected.canonical_json();
    let admission = encoded.reserve(json.len()).unwrap();
    let json = Budgeted::new(json.to_owned(), admission)
        .into_shared()
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let owner = AnalyzerResources::new(crate::AnalyzerLimits::default());
    let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
        let descriptor =
            AnalyzerDescriptor::from_json_with_input(&json, owner.limits(), &mut channel.input())
                .await?;
        drop(json);
        owner
            .restore_image_with_input(descriptor, Vec::new(), &mut channel.input())
            .await
    })
    .unwrap();
    let mut calls = 0;
    let restored = loop {
        calls += 1;
        if let Some(result) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 7 })
            .unwrap()
        {
            break result;
        }
        assert!(calls < 200_000);
    };
    assert!(calls > 1024);
    assert_eq!(
        restored.descriptor().canonical_json(),
        expected.canonical_json()
    );
    assert_eq!(restored.analyze("source").unwrap(), ["alpha", "first"]);
    assert_eq!(encoded.used(), 0);
    drop((input, restored));
    assert_eq!(memory.used(), 0);
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod languages;
