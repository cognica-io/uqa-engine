//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use regex_syntax::hir::{Capture, Hir, Look, Repetition};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("descriptor preparation uses its retained input, not another source read")
    }
}

fn complete<T>(decoder: &mut ImageDecoder<T, AnalysisError>, steps: usize) -> (T, usize) {
    for calls in 1..100_000 {
        if let Some(value) = decoder
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
            .unwrap()
        {
            return (value, calls);
        }
    }
    panic!("finite descriptor work did not complete")
}

fn long_tree() -> (Hir, Vec<u8>) {
    let literal = "a中𠮷".repeat(4097).into_bytes();
    let name = "named_capture".repeat(1700);
    let tree = Hir::concat(vec![
        Hir::look(Look::Start),
        Hir::capture(Capture {
            index: 17,
            name: Some(name.clone().into_boxed_str()),
            sub: Box::new(Hir::repetition(Repetition {
                min: 2,
                max: Some(9),
                greedy: false,
                sub: Box::new(Hir::literal(literal.clone())),
            })),
        }),
        Hir::look(Look::End),
    ]);
    // Independent bytes for this explicit tree in the published v1 preorder format.
    let mut bytes = vec![7];
    bytes.extend(3_u64.to_le_bytes());
    bytes.push(4);
    bytes.extend(Look::Start.as_repr().to_le_bytes());
    bytes.push(6);
    bytes.extend(17_u32.to_le_bytes());
    bytes.push(1);
    bytes.extend((name.len() as u64).to_le_bytes());
    bytes.extend(name.bytes());
    bytes.push(5);
    bytes.extend(2_u32.to_le_bytes());
    bytes.extend([1, 0]);
    bytes.extend(9_u32.to_le_bytes());
    bytes.push(1);
    bytes.extend((literal.len() as u64).to_le_bytes());
    bytes.extend(literal);
    bytes.push(4);
    bytes.extend(Look::End.as_repr().to_le_bytes());
    (tree, bytes)
}

fn wide_tree() -> (Hir, Vec<u8>) {
    let tree = Hir::alternation(
        (1..=5000)
            .map(|index| {
                Hir::capture(Capture {
                    index,
                    name: None,
                    sub: Box::new(Hir::look(Look::End)),
                })
            })
            .collect(),
    );
    let mut bytes = vec![8];
    bytes.extend(5000_u64.to_le_bytes());
    for index in 1_u32..=5000 {
        bytes.push(6);
        bytes.extend(index.to_le_bytes());
        bytes.extend([0, 4]);
        bytes.extend(Look::End.as_repr().to_le_bytes());
    }
    (tree, bytes)
}

#[test]
fn retained_expression_hash_preserves_exact_long_and_wide_native_tree_encoding() {
    for (tree, expected) in [long_tree(), wide_tree()] {
        let tree = Arc::new(tree);
        let expected: [u8; 32] = Sha256::digest(expected).into();
        for steps in [1, 13, 512] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let tree = tree.clone();
                let mut decoder = ImageDecoder::new(&memory, &token, |mut channel| async move {
                    let mut hash = Sha256::new();
                    let word = expression::hash_hir(&tree, &mut hash, &mut channel.input()).await?;
                    Ok::<_, AnalysisError>((<[u8; 32]>::from(hash.finalize()), word))
                })
                .unwrap();
                let ((actual, word), calls) = complete(&mut decoder, steps);
                assert_eq!(actual, expected);
                assert!(!word);
                if steps == 1 {
                    assert!(calls > 20);
                }
                assert!(decoder.advance(&mut NoRead, WINDOW).is_err());
                drop(decoder);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0);
        }
    }
}

fn descriptor() -> Arc<crate::AnalyzerDescriptor> {
    let config = Analyzer {
        tokenizer: Tokenizer::Pattern {
            pattern: "hello".repeat(4000),
        },
        char_filters: (0..200)
            .map(|_| CharFilter::PatternReplace {
                pattern: "hello".to_owned(),
                replacement: "replacement".to_owned(),
            })
            .collect(),
        token_filters: vec![TokenFilter::Lowercase, TokenFilter::ASCIIFolding],
        normalization: None,
    };
    crate::AnalyzerDescriptor::resolve(
        &config,
        crate::TokenLengthPolicy::DiscountOverlaps,
        crate::AnalyzerLimits::default(),
    )
    .unwrap()
}

#[test]
fn retained_descriptor_identity_preserves_canonical_bytes_profiles_and_configuration() {
    let original = descriptor();
    let json: Arc<str> = original.canonical_json().into();
    let config = serde_json::to_value(original.configuration().unwrap()).unwrap();
    let literal_profile = "19646533c4da6bb1fe7a2e586f74910409bbf2508139b3c8e4697dd75fc3704a";
    assert!(original.data.runtime_profiles.expressions[..200]
        .iter()
        .all(|value| value == literal_profile));
    for steps in [1, 17, 512] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let json = json.clone();
            let mut decoder = ImageDecoder::new(&memory, &token, |mut channel| async move {
                crate::AnalyzerDescriptor::from_json_with(
                    &json,
                    crate::AnalyzerLimits::default(),
                    &mut channel.input(),
                )
                .await
            })
            .unwrap();
            if steps == 1 {
                let first = allocation_counter::measure(|| {
                    assert!(decoder.advance(&mut NoRead, WINDOW).unwrap().is_none());
                });
                assert!(first.bytes_total < 64 << 10, "{first:?}");
            }
            let (restored, calls) = complete(&mut decoder, steps);
            if steps == 1 {
                assert!(calls > 400);
            }
            assert_eq!(restored.canonical_json(), original.canonical_json());
            assert_eq!(restored.fingerprint(), original.fingerprint());
            assert_eq!(
                restored.data.runtime_profiles,
                original.data.runtime_profiles
            );
            assert_eq!(
                serde_json::to_value(restored.configuration().unwrap()).unwrap(),
                config
            );
            drop(restored);
            drop(decoder);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

#[test]
fn retained_descriptor_preparation_retires_candidates_on_interruption_and_invalid_windows() {
    let json: Arc<str> = descriptor().canonical_json().into();
    for stop in [0, 1, 8, 64, 256, 400] {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let json = json.clone();
                let mut decoder = ImageDecoder::new(&memory, &token, |mut channel| async move {
                    crate::AnalyzerDescriptor::from_json_with(
                        &json,
                        crate::AnalyzerLimits::default(),
                        &mut channel.input(),
                    )
                    .await
                })
                .unwrap();
                for _ in 0..stop {
                    assert!(decoder.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            decoder.advance(&mut NoRead, WINDOW),
                            Err(AnalysisError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            decoder.advance(&mut NoRead, ImageDecodeWindow { bytes: 0, steps: 1 }),
                            Err(AnalysisError::Input(ImageIoError::Invalid))
                        ));
                    }
                    _ => (),
                }
                if mode != "drop" {
                    assert!(decoder.advance(&mut NoRead, WINDOW).is_err());
                }
                drop(decoder);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "{stop} {mode}");
        }
    }
}

#[test]
fn retained_descriptor_output_avoids_complete_temporary_value_copies_per_poll() {
    let original = descriptor();
    let data = original.data.clone();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut decoder = ImageDecoder::new(&memory, &token, |mut channel| async move {
        crate::AnalyzerDescriptor::finish_with(
            data,
            crate::AnalyzerLimits::default(),
            &mut channel.input(),
        )
        .await
    })
    .unwrap();
    let mut maximum = 0;
    let restored = loop {
        let mut completed = None;
        let measured = allocation_counter::measure(|| {
            completed = decoder.advance(&mut NoRead, WINDOW).unwrap();
        });
        maximum = maximum.max(measured.bytes_total);
        assert!(maximum < 256 << 10, "{measured:?}");
        if let Some(restored) = completed {
            break restored;
        }
    };
    assert_eq!(restored.canonical_json(), original.canonical_json());
    assert_eq!(restored.fingerprint(), original.fingerprint());
    assert_eq!(
        serde_json::to_string(restored.as_ref()).unwrap(),
        original.canonical_json()
    );
    drop(restored);
    drop(decoder);
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_descriptor_output_preserves_reference_bytes_and_limits() {
    let original = descriptor();
    let mut data = original.data.clone();
    data.format = "a\"\u{0000}中𠮷".repeat(1000);
    data.pipeline = serde_json::from_str(&format!(
        "{{\"z\":[{},true,null],\"a\":\"\\u0000\\n𠮷\"}}",
        "9".repeat(20_000)
    ))
    .unwrap();
    data.runtime_profiles.rust_unicode = Some((0, 255, 16));
    for length_policy in [
        crate::TokenLengthPolicy::EmittedTokens,
        crate::TokenLengthPolicy::DiscountOverlaps,
    ] {
        data.length_policy = length_policy;
        let expected = serde_json::to_string(&super::super::canonical(
            &serde_json::to_value(&data).unwrap(),
        ))
        .unwrap();
        let actual = crate::work::inline(super::super::output::data(
            &data,
            crate::AnalyzerLimits::default(),
            &mut (),
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }
    for maximum in [
        1,
        original.canonical_json().len() - 1,
        original.canonical_json().len(),
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let data = original.data.clone();
        let mut decoder = ImageDecoder::new(&memory, &token, |mut channel| async move {
            crate::AnalyzerDescriptor::finish_with(
                data,
                crate::AnalyzerLimits {
                    max_descriptor_bytes: maximum,
                    ..crate::AnalyzerLimits::default()
                },
                &mut channel.input(),
            )
            .await
        })
        .unwrap();
        loop {
            match decoder.advance(&mut NoRead, WINDOW) {
                Ok(None) => (),
                Ok(Some(restored)) => {
                    assert_eq!(maximum, original.canonical_json().len());
                    assert_eq!(restored.canonical_json(), original.canonical_json());
                    break;
                }
                Err(AnalysisError::ResourceLimit {
                    resource,
                    required,
                    limit,
                }) => {
                    assert!(maximum < original.canonical_json().len());
                    assert_eq!(resource, "analyzer descriptor bytes");
                    assert_eq!(limit, maximum);
                    assert!(required > limit);
                    break;
                }
                Err(error) => panic!("unexpected descriptor output error: {error}"),
            }
        }
        assert!(decoder.advance(&mut NoRead, WINDOW).is_err());
        drop(decoder);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_descriptor_output_retires_every_partial_native_owner() {
    let original = descriptor();
    let open = |memory: &MemoryBudget, token: &CancellationToken| {
        let data = original.data.clone();
        ImageDecoder::new(memory, token, |mut channel| async move {
            crate::AnalyzerDescriptor::finish_with(
                data,
                crate::AnalyzerLimits::default(),
                &mut channel.input(),
            )
            .await
        })
        .unwrap()
    };
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut decoder = open(&memory, &token);
    let (restored, calls) = complete(&mut decoder, 1);
    drop(restored);
    drop(decoder);
    for stop in [0, 1, calls / 2, calls - 1] {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut decoder = open(&memory, &token);
                for _ in 0..stop {
                    assert!(decoder.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            decoder.advance(&mut NoRead, WINDOW),
                            Err(AnalysisError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            decoder.advance(&mut NoRead, ImageDecodeWindow { bytes: 0, steps: 1 }),
                            Err(AnalysisError::Input(ImageIoError::Invalid))
                        ));
                    }
                    _ => (),
                }
                drop(decoder);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "{mode} at {stop}");
        }
    }
}
