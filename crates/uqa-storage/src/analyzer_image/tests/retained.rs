//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn decoder(
    resources: AnalyzerResources,
    control: &StorageReadControl,
) -> ImageDecoder<Arc<CompiledAnalyzer>, StorageBackendError> {
    ImageDecoder::new(control.memory(), control.cancellation(), |mut channel| async move {
        read_analyzer_image_from(&mut channel.input(), &resources).await
    })
    .unwrap()
}

struct Source<'a> {
    bytes: Cursor<&'a [u8]>,
    attempts: usize,
    eof: bool,
    interrupted: bool,
}

impl Read for Source<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "preparation must not reread a completed image");
        self.attempts += 1;
        if self.interrupted && self.attempts.is_multiple_of(3) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let count = self.bytes.read(output)?;
        self.eof = count == 0;
        Ok(count)
    }
}

fn source(bytes: &[u8], interrupted: bool) -> Source<'_> {
    Source {
        bytes: Cursor::new(bytes),
        attempts: 0,
        eof: false,
        interrupted,
    }
}

fn advance(
    input: &mut ImageDecoder<Arc<CompiledAnalyzer>, StorageBackendError>,
    source: &mut Source<'_>,
    window: ImageDecodeWindow,
) -> StorageBackendResult<Option<Arc<CompiledAnalyzer>>> {
    let position = source.bytes.position();
    let attempts = source.attempts;
    let result = input.advance(source, window);
    assert!(source.bytes.position() - position <= window.bytes as u64);
    assert!(source.attempts - attempts <= window.steps);
    result
}

fn wide() -> Arc<CompiledAnalyzer> {
    let mut mapping: std::collections::BTreeMap<_, _> = (0..64)
        .map(|i| {
            (
                format!("key{i:04}{}", "한🙂".repeat(128)),
                "value".repeat(256),
            )
        })
        .collect();
    mapping.insert("source".into(), "alpha".into());
    let config = Analyzer::new(
        uqa_analysis::Tokenizer::Keyword,
        vec![uqa_analysis::TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::from([("alpha".into(), vec!["first".into()])]),
            synonyms_path: None,
        }],
        vec![uqa_analysis::CharFilter::Mapping { mapping }],
    );
    config.compile().unwrap()
}

#[test]
fn native_analyzer_input_preserves_original_wire_and_current_read_windows() {
    let original = wide();
    let wire = encode(&original);
    let descriptor = original.descriptor().canonical_json().to_owned();
    drop(original);
    for width in [1, 37, 4096] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        let control = StorageReadControl::with_limit(2 << 20);
        let mut input = decoder(owner.clone(), &control);
        let mut source = source(&wire, width == 37);
        let mut calls = 0;
        let restored = loop {
            calls += 1;
            assert!(calls < 500_000);
            if let Some(value) = advance(
                &mut input,
                &mut source,
                ImageDecodeWindow {
                    bytes: width,
                    steps: 7,
                },
            )
            .unwrap()
            {
                break value;
            }
        };
        assert!(source.eof);
        assert_eq!(source.bytes.position(), wire.len() as u64);
        assert_eq!(restored.descriptor().canonical_json(), descriptor);
        assert_eq!(restored.analyze("source").unwrap(), ["alpha", "first"]);
        assert_eq!(encode(&restored), wire);
        assert_eq!(owner.cache_stats().analyzers, 0);
        drop((restored, input));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_analyzer_input_continues_preparation_after_eof_and_retires_candidates() {
    let wire = encode(&wide());
    for after_eof in [false, true] {
        for action in 0..3 {
            let owner = AnalyzerResources::new(AnalyzerLimits::default());
            let control = StorageReadControl::with_limit(2 << 20);
            let mut input = decoder(owner.clone(), &control);
            let mut source = source(&wire, false);
            let window = ImageDecodeWindow {
                bytes: 4096,
                steps: 1,
            };
            let mut calls = 0;
            loop {
                assert!(advance(&mut input, &mut source, window).unwrap().is_none());
                calls += 1;
                assert!(calls < 100_000);
                if (after_eof && source.eof) || (!after_eof && calls == 32) {
                    break;
                }
            }
            if action == 0 {
                control.cancellation().cancel();
                assert!(matches!(
                    advance(&mut input, &mut source, window),
                    Err(StorageBackendError::Cancelled(_))
                ));
            } else if action == 1 {
                assert!(input
                    .advance(&mut source, ImageDecodeWindow { bytes: 0, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(owner.cache_stats().analyzers, 0);
        }
    }
}

#[test]
fn native_analyzer_input_rejects_corruption_without_escaping_read_or_admission_limits() {
    let wire = encode(&Analyzer::default().compile().unwrap());
    let mut invalid_utf8 = wire.clone();
    invalid_utf8[16] = 255;
    let mut trailing = wire.clone();
    trailing.push(0);
    for bytes in [&wire[..8], &invalid_utf8, &trailing] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = decoder(AnalyzerResources::default(), &control);
        let mut source = source(bytes, false);
        let error = loop {
            match advance(
                &mut input,
                &mut source,
                ImageDecodeWindow { bytes: 1, steps: 2 },
            ) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("corrupt analyzer accepted"),
                Err(error) => break error,
            }
        };
        if bytes == invalid_utf8 {
            assert!(
                matches!(error, StorageBackendError::Other(s) if s == "invalid analyzer image")
            );
        }
        assert_eq!(control.memory().used(), 0);
        assert!(input
            .advance(&mut &wire[..], ImageDecodeWindow { bytes: 9, steps: 2 })
            .is_err());
    }
    let owner = AnalyzerResources::new(AnalyzerLimits {
        max_descriptor_bytes: 1,
        ..Default::default()
    });
    let control = StorageReadControl::with_limit(1 << 20);
    let input = decoder(owner, &control);
    let mut source = source(&wire, false);
    assert!(input.read(&mut source).is_err());
    assert_eq!(source.bytes.position(), 16);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_analyzer_dictionary_input_preserves_exact_images_and_independent_retirement() {
    for (kind, tokenizer, term) in [
        (AnalyzerResourceKind::Nori, "nori_tokenizer", "서울"),
        (AnalyzerResourceKind::Kuromoji, "kuromoji_tokenizer", "東京"),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let config: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let original = owner.compile(&config).unwrap();
        assert_eq!(original.analyze(term).unwrap(), [term]);
        let tokens = original.analyze_tokens(term).unwrap();
        let wire = encode(&original);
        drop((original, owner));
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        let control = StorageReadControl::with_limit(128 << 20);
        let mut input = decoder(owner.clone(), &control);
        let mut source = source(&wire, true);
        let restored = loop {
            if let Some(value) = advance(
                &mut input,
                &mut source,
                ImageDecodeWindow {
                    bytes: 4096,
                    steps: 64,
                },
            )
            .unwrap()
            {
                break value;
            }
        };
        assert!(source.eof);
        assert_eq!(restored.analyze_tokens(term).unwrap(), tokens);
        assert_eq!(encode(&restored), wire);
        assert_eq!(owner.cache_stats().analyzers, 0);
        assert!(control.memory().used() > 0);
        let retained = restored.clone();
        drop((input, restored));
        assert!(control.memory().used() > 0);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}
